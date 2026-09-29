//! Port of src/tmux-utils/pane-dimensions.test.ts

mod common;

use common::{Recorder, healthy_deps, strings, success};
use pretty_assertions::assert_eq;
use tmux_core::{PaneDimensions, get_pane_dimensions};

#[test]
fn delegates_display_to_injected_runner() {
    let recorder = Recorder::new(vec![success("80,160")]);
    let result = get_pane_dimensions("%42", &healthy_deps(&recorder, "sh"));
    assert_eq!(
        result,
        Some(PaneDimensions {
            pane_width: 80,
            window_width: 160
        })
    );
    assert_eq!(
        recorder.calls(),
        vec![(
            "sh".to_owned(),
            strings(&[
                "display",
                "-p",
                "-t",
                "%42",
                "#{pane_width},#{window_width}"
            ])
        )]
    );
}
