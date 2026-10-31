use maho_cli::utils::clipboard_image::*;
#[test]
fn mime_selection_prefers_supported_formats_and_preserves_parameters() {
    let types = ["image/gif", "image/png; charset=binary", "image/jpeg"].map(str::to_owned);
    assert_eq!(
        select_preferred_image_mime_type(&types).as_deref(),
        Some("image/png; charset=binary")
    );
    assert_eq!(
        extension_for_image_mime_type(" IMAGE/JPEG; charset=binary"),
        Some("jpg")
    );
    assert_eq!(extension_for_image_mime_type("text/plain"), None);
}
#[tokio::test]
async fn termux_returns_without_reading_desktop_clipboard() {
    let env = std::collections::BTreeMap::from([("TERMUX_VERSION".to_owned(), "1".to_owned())]);
    assert!(read_clipboard_image(&env, "linux").await.unwrap().is_none());
}

// senpi clipboard-image.test.ts: backend order through the ClipboardIo seam.
mod upstream {
    use maho_cli::utils::clipboard_image::*;
    use std::{collections::BTreeMap, sync::Mutex};

    const PNG: [u8; 16] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52,
    ];
    type Script = Box<dyn Fn(&str, &[&str]) -> Option<Vec<u8>> + Send + Sync>;
    type Native = Box<dyn Fn() -> Result<Option<Vec<u8>>, String> + Send + Sync>;
    struct FakeIo {
        script: Script,
        native: Option<Native>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
        native_reads: Mutex<usize>,
        wsl: bool,
    }
    impl FakeIo {
        fn new(script: impl Fn(&str, &[&str]) -> Option<Vec<u8>> + Send + Sync + 'static) -> Self {
            Self {
                script: Box::new(script),
                native: Some(Box::new(|| Ok(Some(PNG.to_vec())))),
                calls: Mutex::new(Vec::new()),
                native_reads: Mutex::new(0),
                wsl: false,
            }
        }
        fn native(
            mut self,
            answer: impl Fn() -> Result<Option<Vec<u8>>, String> + Send + Sync + 'static,
        ) -> Self {
            self.native = Some(Box::new(answer));
            self
        }
        fn without_native(mut self) -> Self {
            self.native = None;
            self
        }
        fn names(&self) -> Vec<String> {
            self.calls
                .lock()
                .expect("calls")
                .iter()
                .map(|(name, _)| name.clone())
                .collect()
        }
        fn reads(&self) -> usize {
            *self.native_reads.lock().expect("native reads")
        }
    }
    impl ClipboardIo for FakeIo {
        fn run_command<'a>(
            &'a self,
            command: &'a str,
            args: &'a [&'a str],
            _timeout_ms: Option<u64>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send + 'a>>
        {
            self.calls.lock().expect("calls").push((
                command.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            ));
            let answer = (self.script)(command, args);
            Box::pin(async move { answer })
        }
        fn native_image(&self) -> Option<Result<Option<Vec<u8>>, String>> {
            let native = self.native.as_ref()?;
            *self.native_reads.lock().expect("native reads") += 1;
            Some(native())
        }
        fn is_wsl(&self, _env: &BTreeMap<String, String>) -> bool {
            self.wsl
        }
    }
    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }
    fn image(result: Option<ClipboardImage>) -> Option<(Vec<u8>, String)> {
        result.map(|image| (image.bytes, image.mime_type))
    }

    #[tokio::test]
    async fn command_backends_with_or_without_an_image_stop_the_fallback() {
        for (command, environment) in [
            (
                "wl-paste",
                env(&[("WAYLAND_DISPLAY", "1"), ("DISPLAY", ":0")]),
            ),
            ("xclip", env(&[("DISPLAY", ":0")])),
        ] {
            for present in [true, false] {
                let io = FakeIo::new(move |name, args| {
                    assert_eq!(name, command);
                    let listing = args.contains(&"--list-types") || args.contains(&"TARGETS");
                    Some(if listing {
                        if present {
                            b"text/plain\nimage/png\n".to_vec()
                        } else {
                            b"text/plain\n".to_vec()
                        }
                    } else {
                        PNG.to_vec()
                    })
                });
                let result = read_clipboard_image_with(&environment, "linux", &io)
                    .await
                    .unwrap();
                assert_eq!(
                    image(result),
                    present.then(|| (PNG.to_vec(), "image/png".to_owned())),
                    "{command} {present}"
                );
                assert_eq!(io.names().len(), if present { 2 } else { 1 });
                assert_eq!(io.reads(), 0);
            }
        }
    }
    #[tokio::test]
    async fn x11_does_not_probe_image_types_when_targets_fails() {
        let io = FakeIo::new(|_, args| {
            if args.contains(&"TARGETS") {
                None
            } else {
                Some(b"hello".to_vec())
            }
        })
        .native(|| Ok(None));
        assert!(
            read_clipboard_image_with(&env(&[("DISPLAY", ":0")]), "linux", &io)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            *io.calls.lock().unwrap(),
            vec![(
                "xclip".to_owned(),
                ["-selection", "clipboard", "-t", "TARGETS", "-o"]
                    .map(str::to_owned)
                    .to_vec()
            )]
        );
        assert_eq!(io.reads(), 1);
    }
    #[tokio::test]
    async fn x11_does_not_probe_unadvertised_image_types() {
        let io = FakeIo::new(|_, args| {
            if args.contains(&"TARGETS") {
                Some(b"image/png\n".to_vec())
            } else if args.contains(&"image/png") {
                None
            } else {
                Some(b"hello".to_vec())
            }
        })
        .native(|| Ok(None));
        assert!(
            read_clipboard_image_with(&env(&[("DISPLAY", ":0")]), "linux", &io)
                .await
                .unwrap()
                .is_none()
        );
        let probed: Vec<String> = io
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, args)| args[3].clone())
            .collect();
        assert_eq!(probed, ["TARGETS", "image/png"]);
        assert_eq!(io.reads(), 1);
    }
    #[tokio::test]
    async fn the_native_x11_result_stops_the_fallback() {
        for bytes in [Some(PNG.to_vec()), None, Some(Vec::new())] {
            let answer = bytes.clone();
            let io = FakeIo::new(|_, _| None).native(move || Ok(answer.clone()));
            let result = read_clipboard_image_with(&env(&[("DISPLAY", ":0")]), "linux", &io)
                .await
                .unwrap();
            assert_eq!(
                image(result),
                bytes
                    .clone()
                    .filter(|bytes| !bytes.is_empty())
                    .map(|bytes| (bytes, "image/png".to_owned()))
            );
            assert_eq!(io.reads(), 1);
            assert_eq!(io.names(), ["xclip"]);
        }
    }
    #[tokio::test]
    async fn wayland_falls_back_to_x11_after_a_failed_listing() {
        let io = FakeIo::new(|name, args| {
            if name == "wl-paste" {
                None
            } else if args.contains(&"TARGETS") {
                Some(b"image/png\n".to_vec())
            } else {
                Some(PNG.to_vec())
            }
        })
        .without_native();
        let result = read_clipboard_image_with(&env(&[("WAYLAND_DISPLAY", "1")]), "linux", &io)
            .await
            .unwrap();
        assert_eq!(image(result), Some((PNG.to_vec(), "image/png".to_owned())));
        assert_eq!(io.reads(), 0);
    }
    #[tokio::test]
    async fn wsl_tries_powershell_before_a_broken_native_x11_bridge() {
        let target = std::sync::Arc::new(Mutex::new(None::<String>));
        let seen = target.clone();
        let mut io = FakeIo::new(move |name, args| match name {
            "wl-paste" | "xclip" => None,
            "wslpath" => {
                *seen.lock().unwrap() = Some(args[1].to_owned());
                Some(b"C:\\Users\\O'Hare\\clip.png\n".to_vec())
            }
            "powershell.exe" => {
                assert!(args[2].contains("$path = 'C:\\Users\\O''Hare\\clip.png'"));
                std::fs::write(seen.lock().unwrap().as_deref().expect("wslpath first"), PNG)
                    .unwrap();
                Some(b"ok\n".to_vec())
            }
            other => panic!("unexpected command {other}"),
        })
        .native(|| Err("Broken X11 bridge".to_owned()));
        io.wsl = true;
        let result =
            read_clipboard_image_with(&env(&[("WSL_DISTRO_NAME", "Ubuntu")]), "linux", &io)
                .await
                .unwrap();
        assert_eq!(image(result), Some((PNG.to_vec(), "image/png".to_owned())));
        assert_eq!(io.reads(), 0);
    }
    #[tokio::test]
    async fn darwin_and_windows_read_the_native_image_once() {
        for platform in ["darwin", "win32"] {
            for bytes in [Some(PNG.to_vec()), None, Some(Vec::new())] {
                let answer = bytes.clone();
                let io =
                    FakeIo::new(|_, _| panic!("no commands")).native(move || Ok(answer.clone()));
                let result = read_clipboard_image_with(&BTreeMap::new(), platform, &io)
                    .await
                    .unwrap();
                assert_eq!(
                    image(result),
                    bytes
                        .filter(|bytes| !bytes.is_empty())
                        .map(|bytes| (bytes, "image/png".to_owned()))
                );
                assert_eq!(io.reads(), 1);
            }
        }
        let io = FakeIo::new(|_, _| panic!("no commands")).without_native();
        assert!(
            read_clipboard_image_with(&BTreeMap::new(), "win32", &io)
                .await
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn native_transfer_errors_propagate_without_fallback() {
        for platform in ["linux", "win32"] {
            let io = FakeIo::new(|_, _| None)
                .native(|| Err("Native clipboard operation failed".to_owned()));
            let error = read_clipboard_image_with(
                &env(&[("WAYLAND_DISPLAY", "1"), ("DISPLAY", ":0")]),
                platform,
                &io,
            )
            .await
            .err();
            assert_eq!(error.as_deref(), Some("Native clipboard operation failed"));
            assert_eq!(
                io.names(),
                if platform == "linux" {
                    vec!["wl-paste".to_owned(), "xclip".to_owned()]
                } else {
                    Vec::new()
                }
            );
        }
    }
    #[tokio::test]
    async fn termux_reads_nothing() {
        let io = FakeIo::new(|_, _| panic!("no commands"));
        assert!(
            read_clipboard_image_with(&env(&[("TERMUX_VERSION", "0.119")]), "linux", &io)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(io.reads(), 0);
    }

    // senpi clipboard-image-bmp-conversion.test.ts: a BMP from wl-paste (linux) or the native
    // clipboard (win32) comes back as PNG.
    fn tiny_bmp_1x1_red_24bpp() -> Vec<u8> {
        let mut buffer = vec![0u8; 58];
        buffer[0..2].copy_from_slice(b"BM");
        buffer[2..6].copy_from_slice(&58u32.to_le_bytes());
        buffer[10..14].copy_from_slice(&54u32.to_le_bytes());
        buffer[14..18].copy_from_slice(&40u32.to_le_bytes());
        buffer[18..22].copy_from_slice(&1i32.to_le_bytes());
        buffer[22..26].copy_from_slice(&1i32.to_le_bytes());
        buffer[26..28].copy_from_slice(&1u16.to_le_bytes());
        buffer[28..30].copy_from_slice(&24u16.to_le_bytes());
        buffer[34..38].copy_from_slice(&4u32.to_le_bytes());
        buffer[56] = 0xff;
        buffer
    }
    #[tokio::test]
    async fn command_and_native_bmp_images_are_converted_to_png() {
        for platform in ["linux", "win32"] {
            let io = FakeIo::new(|name, args| {
                if name == "wl-paste" && args.contains(&"--list-types") {
                    Some(b"image/bmp\n".to_vec())
                } else if name == "wl-paste" && args.contains(&"image/bmp") {
                    Some(tiny_bmp_1x1_red_24bpp())
                } else {
                    None
                }
            })
            .native(|| Ok(Some(tiny_bmp_1x1_red_24bpp())));
            let image =
                read_clipboard_image_with(&env(&[("WAYLAND_DISPLAY", "wayland-0")]), platform, &io)
                    .await
                    .unwrap()
                    .expect("converted image");
            assert_eq!(image.mime_type, "image/png", "{platform}");
            assert_eq!(image.bytes[..4], [0x89, 0x50, 0x4e, 0x47], "{platform}");
        }
    }
}
