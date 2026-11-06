mod history;
mod segment;

pub use segment::{Format, Limits, Note, NoteKind, detect_format};

// Temporarily public: the CLI driver still consumes these directly.
pub use history::select_history;
pub use segment::{FinishError, Issue, Segment, Unit, split, split_unit};
