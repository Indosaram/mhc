use crate::internal::validate::{Node, optional, strict_object, string};

/// How a client draws the four builtin model-profile lanes as a 2x2 pad. The engine never reads it:
/// it is presentation shared by every client through omo.json, so the desktop and any later
/// surface show the same words. A square's own name and model stay in `model_profiles.<lane id>`.
///
/// `labels` renames the pad's axes: `daily` / `geeky` are the columns, `heavy` / `normal` the rows.
/// An unset label keeps the builtin word.
fn omo_model_pad_labels_schema() -> Node {
    strict_object(vec![
        optional("daily", string()),
        optional("geeky", string()),
        optional("normal", string()),
        optional("heavy", string()),
    ])
}

pub fn omo_model_pad_schema() -> Node {
    strict_object(vec![optional("labels", omo_model_pad_labels_schema())])
}
