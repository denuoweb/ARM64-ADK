#[derive(Clone, Debug)]
pub(crate) struct ProjectTemplateOption {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolchainSetOption {
    pub(crate) id: String,
    pub(crate) label: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TargetOption {
    pub(crate) id: String,
    pub(crate) label: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ActiveContext {
    pub(crate) run_id: String,
    pub(crate) project_id: String,
    pub(crate) project_path: String,
    pub(crate) toolchain_set_id: String,
    pub(crate) target_id: String,
}

impl ActiveContext {
    pub(crate) fn project_ref(&self) -> String {
        if !self.project_id.trim().is_empty() {
            self.project_id.clone()
        } else {
            self.project_path.clone()
        }
    }
}
