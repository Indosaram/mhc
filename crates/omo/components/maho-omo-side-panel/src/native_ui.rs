use crate::{
    constants::MIN_TRANSCRIPT_COLUMNS,
    links::parse_panel_action_url,
    popups::text_popup::{PopupInput, TextPopup},
    types::*,
};
use maho_ext_api::{ExtensionUi, NotificationType};
use std::sync::{Arc, Mutex};

pub struct NativePanelUi(pub Arc<dyn ExtensionUi>);
pub fn theme_adapter(theme: &maho_ext_api::Theme) -> Theme {
    let theme = theme.clone();
    Arc::new(move |color, text| theme.fg(color.as_str(), text))
}
pub struct NativeBody {
    pub body: Component,
    pub action: Option<ActionHook>,
    pub lines: Vec<String>,
}
impl maho_tui::tui::Component for NativeBody {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.lines = self.body.render(width);
        self.lines.clone()
    }
    fn invalidate(&mut self) {
        self.body.invalidate();
    }
    fn handle_mouse(
        &mut self,
        event: &maho_tui::tui::TuiMouseEvent,
    ) -> Option<maho_tui::tui::TuiMouseEventResult> {
        use maho_tui::tui::{TuiMouseButton, TuiMouseEventResult, TuiMouseEventType};
        if event.event_type != TuiMouseEventType::Click || event.button != TuiMouseButton::Left {
            return None;
        }
        let action = self.action.as_ref()?;
        let line = self.lines.get(usize::try_from(event.y).ok()?)?;
        let url = maho_tui::utils::get_osc8_link_at_column(line, usize::try_from(event.x).ok()?)?;
        action(parse_panel_action_url(&url)?);
        Some(TuiMouseEventResult {
            handled: true,
            render: Some(true),
            ..Default::default()
        })
    }
}
impl PanelUi for NativePanelUi {
    fn panel_theme(&self) -> Option<Theme> {
        Some(theme_adapter(&self.0.theme()))
    }
    fn mount_native_column(
        &self,
        body: Component,
        width: Arc<dyn Fn(usize) -> usize + Send + Sync>,
        min_columns: usize,
        action: Option<ActionHook>,
    ) -> bool {
        let result = self
            .0
            .set_side_column(Some(maho_ext_api::ExtensionSideColumn {
                factory: Arc::new(move |_| {
                    Box::new(NativeBody {
                        body: body.clone(),
                        action: action.clone(),
                        lines: Vec::new(),
                    })
                }),
                width,
                min_columns,
                min_transcript_columns: MIN_TRANSCRIPT_COLUMNS,
            }));
        match result {
            Ok(()) => true,
            Err(error) => {
                self.0.notify(&error.to_string(), NotificationType::Warning);
                false
            }
        }
    }
    fn clear_native_column(&self) {
        if let Err(error) = self.0.set_side_column(None) {
            self.0.notify(&error.to_string(), NotificationType::Warning);
        }
    }
    fn repaint_native_column(&self) {
        if let Err(error) = self.0.request_render() {
            self.0.notify(&error.to_string(), NotificationType::Warning);
        }
    }
    fn set_widget(&self, key: &str, factory: Option<WidgetFactory>, above: bool) {
        if factory.is_some() {
            self.0.notify(
                "The native panel requires a column-capable interactive host",
                NotificationType::Warning,
            );
        } else {
            self.0.set_widget(
                key,
                None,
                maho_ext_api::ExtensionWidgetOptions {
                    placement: if above {
                        maho_ext_api::WidgetPlacement::AboveEditor
                    } else {
                        maho_ext_api::WidgetPlacement::BelowEditor
                    },
                    actions: None,
                },
            );
        }
    }
    fn notify(&self, message: &str, kind: &str) {
        self.0.notify(
            message,
            match kind {
                "error" => NotificationType::Error,
                "warning" => NotificationType::Warning,
                _ => NotificationType::Info,
            },
        );
    }
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: Vec<String>,
    ) -> PanelFuture<'a, Option<String>> {
        Box::pin(async move { self.0.select(title, &options, Default::default()).await })
    }
    fn viewer<'a>(&'a self, title: &'a str, rows: Vec<PanelRow>) -> PanelFuture<'a, ()> {
        let title = title.to_owned();
        Box::pin(async move {
            let popup = Arc::new(Mutex::new(TextPopup::new(title)));
            let wheel_popup = popup.clone();
            let ui = self.0.clone();
            let _wheel = TerminalInputGuard(self.on_terminal_input(Arc::new(move |data| {
                if crate::popups::wheel::parse_panel_wheel_event(data).is_none() {
                    return false;
                }
                wheel_popup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .handle_input(data);
                if let Err(error) = ui.request_render() {
                    ui.notify(&error.to_string(), NotificationType::Warning);
                }
                true
            })));
            let factory: maho_ext_api::CustomComponentFactory =
                Arc::new(move |host, theme, _, done| {
                    let popup = NativePopup {
                        popup: popup.clone(),
                        rows: rows.clone(),
                        theme: theme_adapter(theme),
                        dimensions: host.dimensions_reader(),
                        done,
                    };
                    Box::pin(
                        async move { Ok(Box::new(popup) as Box<dyn maho_tui::tui::Component>) },
                    )
                });
            if let Err(error) = self
                .0
                .custom_factory(
                    factory,
                    maho_ext_api::CustomUiFactoryOptions {
                        overlay: true,
                        overlay_options: Some(maho_ext_api::ExtensionOverlayOptions::Static(
                            Arc::new(|| maho_tui::tui::OverlayOptions {
                                width: Some(maho_tui::tui::SizeValue::Percent(70.0)),
                                min_width: Some(52),
                                max_height: Some(maho_tui::tui::SizeValue::Percent(100.0)),
                                ..Default::default()
                            }),
                        )),
                        ..Default::default()
                    },
                )
                .await
            {
                self.0.notify(&error.to_string(), NotificationType::Error);
            }
        })
    }
    fn on_terminal_input(&self, handler: TerminalHandler) -> Option<Stop> {
        match self.0.on_terminal_input(Arc::new(move |data| {
            handler(data).then_some(maho_ext_api::TerminalInputResult {
                consume: Some(true),
                data: None,
            })
        })) {
            Ok(stop) => Some(stop),
            Err(error) => {
                self.0.notify(&error.to_string(), NotificationType::Warning);
                None
            }
        }
    }
}
struct TerminalInputGuard(Option<Stop>);
impl Drop for TerminalInputGuard {
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            stop();
        }
    }
}
struct NativePopup {
    popup: Arc<Mutex<TextPopup>>,
    rows: Vec<PanelRow>,
    theme: Theme,
    dimensions: std::rc::Rc<dyn Fn() -> (u16, u16)>,
    done: maho_ext_api::CustomUiDone,
}
impl maho_tui::tui::Component for NativePopup {
    fn handle_mouse(
        &mut self,
        event: &maho_tui::tui::TuiMouseEvent,
    ) -> Option<maho_tui::tui::TuiMouseEventResult> {
        if event.event_type != maho_tui::tui::TuiMouseEventType::Wheel {
            return None;
        }
        let delta = event.wheel_delta?;
        let amount = usize::try_from(delta.unsigned_abs())
            .unwrap_or(usize::MAX)
            .saturating_mul(3);
        let mut popup = self
            .popup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        popup.scroll = if delta < 0 {
            popup.scroll.saturating_sub(amount)
        } else {
            popup.scroll.saturating_add(amount)
        };
        Some(maho_tui::tui::TuiMouseEventResult {
            handled: true,
            render: Some(true),
            ..Default::default()
        })
    }
    fn render(&mut self, width: usize) -> Vec<String> {
        self.popup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .render(
                width,
                usize::from((self.dimensions)().1),
                &self.rows,
                Some(&self.theme),
            )
    }
    fn handle_input(&mut self, data: &str) {
        let result = self
            .popup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .handle_input(data);
        if result == PopupInput::Close {
            (self.done)(serde_json::Value::Null);
        }
    }
    fn has_input_handler(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_tui::tui::{Component as _, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};

    struct Body;
    impl PanelComponent for Body {
        fn render(&self, _: usize) -> Vec<String> {
            vec!["plain \x1b]8;;omo-panel:goal/current\x07goal\x1b]8;;\x07 tail".into()]
        }
    }

    #[test]
    fn native_click_dispatches_only_the_link_under_its_column() {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let sink = actions.clone();
        let mut body = NativeBody {
            body: Arc::new(Body),
            action: Some(Arc::new(move |action| {
                sink.lock().expect("actions").push(action)
            })),
            lines: Vec::new(),
        };
        body.render(40);
        let mut click = TuiMouseEvent {
            event_type: TuiMouseEventType::Click,
            button: TuiMouseButton::Left,
            x: 0,
            y: 0,
            screen_x: 0,
            screen_y: 0,
            width: 40,
            height: 1,
            shift: false,
            alt: false,
            ctrl: false,
            wheel_delta: None,
            click_count: Some(1),
        };
        assert!(body.handle_mouse(&click).is_none());
        click.x = 6;
        assert!(body.handle_mouse(&click).is_some());
        click.x = 10;
        assert!(body.handle_mouse(&click).is_none());
        assert_eq!(
            *actions.lock().expect("actions"),
            vec![crate::links::PanelAction::Goal]
        );
    }

    #[test]
    fn wheel_subscription_is_disposed_when_viewer_scope_ends() {
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mark = stopped.clone();
        {
            let _guard = TerminalInputGuard(Some(Box::new(move || {
                mark.store(true, std::sync::atomic::Ordering::SeqCst);
            })));
            assert!(!stopped.load(std::sync::atomic::Ordering::SeqCst));
        }
        assert!(stopped.load(std::sync::atomic::Ordering::SeqCst));
    }
}
