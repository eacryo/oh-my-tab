use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub enum SwitcherState {
    Idle,
    Overlay { selected: usize },
    Navigating { selected: usize },
}

impl Default for SwitcherState {
    fn default() -> Self {
        SwitcherState::Idle
    }
}
