use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub enum SwitcherState {
    #[default]
    Idle,
    Overlay {
        selected: usize,
    },
    Navigating {
        selected: usize,
    },
}
