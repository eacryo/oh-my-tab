use std::sync::mpsc::Sender;

#[derive(Debug, Clone)]
pub enum KeyEvent {
    CmdDown,
    CmdUp,
    TabDown,
    TabUp,
    ShiftDown,
    ShiftUp,
    EscDown,
}

pub fn start_event_monitor(_tx: Sender<KeyEvent>) {
    println!("[event_monitor] Event tap will be set up in Step 3");
}
