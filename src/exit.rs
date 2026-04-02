#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Success = 0,
    Usage = 2,
    Config = 3,
    Session = 4,
    SessionConflict = 5,
    Provider = 6,
    Timeout = 7,
    Tool = 8,
    Shell = 9,
    Runtime = 10,
}

impl ExitCode {
    pub const fn code(self) -> i32 {
        self as i32
    }
}
