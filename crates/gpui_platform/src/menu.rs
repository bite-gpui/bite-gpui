/// OS actions are actions that are recognized by the operating system
/// This allows the operating system to provide specialized behavior for
/// these actions
#[derive(Copy, Clone, Eq, PartialEq)]
pub enum OsAction {
    /// The 'cut' action
    Cut,

    /// The 'copy' action
    Copy,

    /// The 'paste' action
    Paste,

    /// The 'select all' action
    SelectAll,

    /// The 'undo' action
    Undo,

    /// The 'redo' action
    Redo,
}
