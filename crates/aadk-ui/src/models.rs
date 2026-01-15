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
