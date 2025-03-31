use crate::*;
use dap::DebugRequestType;
use gpui::AsyncApp;
use std::{ffi::OsStr, path::PathBuf};
use task::DebugTaskDefinition;

#[derive(Default)]
pub(crate) struct PythonDebugAdapter;

impl PythonDebugAdapter {
    const ADAPTER_NAME: &'static str = "Debugpy";
    const ADAPTER_PACKAGE_NAME: &'static str = "debugpy";
    const ADAPTER_PATH: &'static str = "src/debugpy/adapter";
    const LANGUAGE_NAME: &'static str = "Python";
}

#[async_trait(?Send)]
impl DebugAdapter for PythonDebugAdapter {
    fn name(&self) -> DebugAdapterName {
        DebugAdapterName(Self::ADAPTER_NAME.into())
    }

    async fn fetch_latest_adapter_version(
        &self,
        delegate: &dyn DapDelegate,
    ) -> Result<AdapterVersion> {
        let github_repo = GithubRepo {
            repo_name: Self::ADAPTER_PACKAGE_NAME.into(),
            repo_owner: "microsoft".into(),
        };

        adapters::fetch_latest_adapter_version_from_github(github_repo, delegate).await
    }

    async fn install_binary(
        &self,
        version: AdapterVersion,
        delegate: &dyn DapDelegate,
    ) -> Result<()> {
        let version_path = adapters::download_adapter_from_github(
            self.name(),
            version,
            adapters::DownloadedFileType::Zip,
            delegate,
        )
        .await?;

        // only needed when you install the latest version for the first time
        if let Some(debugpy_dir) =
            util::fs::find_file_name_in_dir(version_path.as_path(), |file_name| {
                file_name.starts_with("microsoft-debugpy-")
            })
            .await
        {
            // TODO Debugger: Rename folder instead of moving all files to another folder
            // We're doing unnecessary IO work right now
            util::fs::move_folder_files_to_folder(debugpy_dir.as_path(), version_path.as_path())
                .await?;
        }

        Ok(())
    }

    async fn get_installed_binary(
        &self,
        delegate: &dyn DapDelegate,
        config: &DebugAdapterConfig,
        user_installed_path: Option<PathBuf>,
        cx: &mut AsyncApp,
    ) -> Result<DebugAdapterBinary> {
        const BINARY_NAMES: [&str; 3] = ["python3", "python", "py"];
        let tcp_connection = config.tcp_connection.clone().unwrap_or_default();
        let (host, port, timeout) = crate::configure_tcp_connection(tcp_connection).await?;

        let debugpy_dir = if let Some(user_installed_path) = user_installed_path {
            user_installed_path
        } else {
            let adapter_path = paths::debug_adapters_dir().join(self.name().as_ref());
            let file_name_prefix = format!("{}_", Self::ADAPTER_PACKAGE_NAME);

            util::fs::find_file_name_in_dir(adapter_path.as_path(), |file_name| {
                file_name.starts_with(&file_name_prefix)
            })
            .await
            .ok_or_else(|| anyhow!("Debugpy directory not found"))?
        };

        let toolchain = delegate
            .toolchain_store()
            .active_toolchain(
                delegate.worktree_id(),
                language::LanguageName::new(Self::LANGUAGE_NAME),
                cx,
            )
            .await;

        let python_path = match toolchain { Some(toolchain) => {
            Some(toolchain.path.to_string())
        } _ => {
            BINARY_NAMES
                .iter()
                .filter_map(|cmd| {
                    delegate
                        .which(OsStr::new(cmd))
                        .map(|path| path.to_string_lossy().to_string())
                })
                .find(|_| true)
        }};

        Ok(DebugAdapterBinary {
            command: python_path.ok_or(anyhow!("failed to find binary path for python"))?,
            arguments: Some(vec![
                debugpy_dir.join(Self::ADAPTER_PATH).into(),
                format!("--port={}", port).into(),
                format!("--host={}", host).into(),
            ]),
            connection: Some(adapters::TcpArguments {
                host,
                port,
                timeout,
            }),
            cwd: None,
            envs: None,
        })
    }

    fn request_args(&self, config: &DebugTaskDefinition) -> Value {
        match &config.request {
            DebugRequestType::Launch(launch_config) => {
                json!({
                    "program": launch_config.program,
                    "subProcess": true,
                    "cwd": launch_config.cwd,
                    "redirectOutput": true,
                })
            }
            dap::DebugRequestType::Attach(attach_config) => {
                json!({
                    "subProcess": true,
                    "redirectOutput": true,
                    "processId": attach_config.process_id
                })
            }
        }
    }
}
