//! A separate settings window; applying preferences never performs filesystem IO.
use crate::cache::CacheSettings;
use crossbeam_channel::Sender;
#[cfg(windows)]
use gpui_kit::component::Disableable;
use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        checkbox::Checkbox,
        input::{Input, InputState},
        Sizable,
    },
    prelude::*,
    *,
};
use std::{path::PathBuf, time::Duration};

pub enum Command {
    Apply(CacheSettings),
    ClearDisk,
}

pub fn open(
    config: CacheSettings,
    commands: Sender<Command>,
    cx: &mut App,
) -> anyhow::Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(720.), px(760.)), cx);
    let (handle, _) = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(620.), px(580.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Scrutivue — Settings".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
        move |window, cx| {
            crate::platform::style_window(window);
            cx.new(|cx| {
                let ram = cx.new(|cx| {
                    InputState::new(window, cx).default_value(config.ram_mib.to_string())
                });
                let disk = cx.new(|cx| {
                    InputState::new(window, cx).default_value(config.disk_mib.to_string())
                });
                let path = cx.new(|cx| {
                    InputState::new(window, cx)
                        .default_value(config.disk_path.to_string_lossy().into_owned())
                });
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                let (folder_tx, folder_rx) = crossbeam_channel::bounded::<Option<PathBuf>>(1);
                cx.spawn_in(window, async move |this, cx| loop {
                    smol::Timer::after(Duration::from_millis(30)).await;
                    if this
                        .update_in(cx, |this: &mut Pane, window, cx| {
                            if let Ok(folder) = this.folder_rx.try_recv() {
                                this.choosing = false;
                                if let Some(folder) = folder {
                                    this.path.update(cx, |input, cx| {
                                        input.set_value(
                                            folder.to_string_lossy().into_owned(),
                                            window,
                                            cx,
                                        )
                                    });
                                }
                                cx.notify();
                            }
                            #[cfg(windows)]
                            while let Ok(result) = this.integration.events.try_recv() {
                                this.integration_busy = false;
                                this.integration_message = match result {
                                    Ok(message) | Err(message) => message,
                                };
                                cx.notify();
                            }
                        })
                        .is_err()
                    {
                        break;
                    }
                })
                .detach();
                Pane {
                    ram,
                    disk,
                    path,
                    config,
                    commands,
                    focus,
                    message: String::new(),
                    folder_tx,
                    folder_rx,
                    choosing: false,
                    #[cfg(windows)]
                    integration: crate::windows_integration::Worker::new(),
                    #[cfg(windows)]
                    integration_busy: true,
                    #[cfg(windows)]
                    integration_message: "Checking Open with registration…".into(),
                }
            })
        },
    )?;
    Ok(handle)
}
struct Pane {
    ram: Entity<InputState>,
    disk: Entity<InputState>,
    path: Entity<InputState>,
    config: CacheSettings,
    commands: Sender<Command>,
    focus: FocusHandle,
    message: String,
    folder_tx: Sender<Option<PathBuf>>,
    folder_rx: crossbeam_channel::Receiver<Option<PathBuf>>,
    choosing: bool,
    #[cfg(windows)]
    integration: crate::windows_integration::Worker,
    #[cfg(windows)]
    integration_busy: bool,
    #[cfg(windows)]
    integration_message: String,
}
impl Pane {
    #[cfg(windows)]
    fn integration_action(
        &mut self,
        action: crate::windows_integration::Action,
        cx: &mut Context<Self>,
    ) {
        match self.integration.request(action) {
            Ok(()) => {
                self.integration_busy = true;
                self.integration_message = "Updating Open with registration…".into();
            }
            Err(message) => self.integration_message = message,
        }
        cx.notify();
    }
    fn integration_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        #[cfg(windows)]
        {
            div()
                .child(heading("Windows Open with"))
                .child(note("Offer this executable as a viewer for supported image extensions for the current Windows user. Opening an image selects it and lists other supported images in its folder. No administrator rights are needed; default apps are not changed. Register again after moving or updating the application."))
                .child(div().flex().gap_3()
                    .child(Button::new("windows-open-with-register").label("Register").small().disabled(self.integration_busy)
                        .tooltip("Add Scrutivue to Windows Open with for supported images")
                        .on_click(cx.listener(|this,_,_,cx| this.integration_action(crate::windows_integration::Action::Register,cx))))
                    .child(Button::new("windows-open-with-remove").label("Remove").small().ghost().disabled(self.integration_busy)
                        .tooltip("Remove this executable's Open with registration without changing default apps")
                        .on_click(cx.listener(|this,_,_,cx| this.integration_action(crate::windows_integration::Action::Remove,cx)))))
                .child(div().mt_2().text_xs().text_color(rgb(0xb4b4ba)).child(self.integration_message.clone()))
                .into_any_element()
        }
        #[cfg(not(windows))]
        {
            let _ = cx;
            div().into_any_element()
        }
    }
    fn apply(&mut self, cx: &mut Context<Self>) {
        let result = (|| -> anyhow::Result<CacheSettings> {
            let ram = self.ram.read(cx).value().trim().parse::<u64>()?;
            let disk = self.disk.read(cx).value().trim().parse::<u64>()?;
            anyhow::ensure!(
                (64..=1_048_576).contains(&ram),
                "RAM cache must be between 64 and 1048576 MiB"
            );
            anyhow::ensure!(
                disk <= 1_048_576,
                "Disk cache must be between 0 and 1048576 MiB"
            );
            let path = PathBuf::from(self.path.read(cx).value().trim());
            anyhow::ensure!(path.is_absolute(), "Choose an absolute disk cache location");
            Ok(CacheSettings {
                ram_mib: ram,
                disk_mib: disk,
                disk_path: path,
                ..self.config.clone()
            })
        })();
        self.message = match result {
            Ok(config) => {
                self.config = config.clone();
                match self.commands.try_send(Command::Apply(config)) {
                    Ok(()) => "Settings applied. Cache work runs in the background.".into(),
                    Err(_) => "Settings could not be applied; try again.".into(),
                }
            }
            Err(error) => format!("Invalid settings: {error}"),
        };
        cx.notify();
    }
}
impl Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().flex().flex_col().bg(rgb(0x232325)).text_color(rgb(0xe5e5e7)).text_sm().font_family("Segoe UI").track_focus(&self.focus)
            .on_key_down(cx.listener(|_,event:&KeyDownEvent,window,_|if event.keystroke.key=="escape" {window.remove_window();}))
            .child(div().id("settings-content").flex_1().min_h_0().overflow_y_scroll().p_6()
            .child(div().text_xl().mb_5().child("Settings"))
            .child(heading("Memory cache"))
            .child(div().flex().items_center().gap_4()
                .child(div().w(px(150.)).child("Total RAM cache (MiB)"))
                .child(div().w(px(120.)).child(Input::new(&self.ram).aria_label("Total RAM cache in MiB")))
                .child(Checkbox::new("cache-prefetch").label("Prefetch").checked(self.config.prefetch).on_click(cx.listener(|this,checked,_,cx|{this.config.prefetch=*checked;cx.notify();}))))
            .child(note("50% for complete RGB16 frames; 50% for decoded display previews. Active A/B frames, decoding, analysis and GPU memory are additional. Prefetch reads neighboring frames only after the selection is ready."))
            .child(heading("Disk preview cache"))
            .child(div().flex().items_center().gap_4().child(div().w(px(150.)).child("Maximum size (MiB)"))
                .child(div().w(px(120.)).child(Input::new(&self.disk).aria_label("Maximum disk preview cache in MiB"))))
            .child(note("Default: 1024 MiB. Set 0 to disable disk storage. Only images of at least 10 megapixels are eligible."))
            .child(div().mb_2().child("Location"))
            .child(div().flex().items_center().gap_2().child(div().flex_1().min_w_0().child(Input::new(&self.path).aria_label("Disk preview cache location")))
                .child(Button::new("cache-browse").label("Browse…").small().on_click(cx.listener(|this,_,_,cx| {
                    if !this.choosing {this.choosing=true;let tx=this.folder_tx.clone();std::thread::spawn(move||{let _=tx.send(rfd::FileDialog::new().set_title("Choose preview cache location").pick_folder());});cx.notify();}
                }))))
            .child(note("An application-owned subfolder contains a SHA-256 hash tree. Previews: 2560 px long edge, JPEG XL distance 0.05, effort 3. Lossy previews are never used for exact tools."))
            .child(Checkbox::new("cache-all-files").label("Build previews for all added files").checked(self.config.build_all).on_click(cx.listener(|this,checked,_,cx|{this.config.build_all=*checked;cx.notify();})))
            .child(note("Build lazily in the background while selected images are ready. Otherwise previews are created only by viewing or prefetching images."))
            .child(Button::new("cache-clear").label("Clear disk preview cache").small().on_click(cx.listener(|this,_,_,cx| {
                this.message=if this.commands.try_send(Command::ClearDisk).is_ok() {"Cache reset requested for the currently applied location. Progress/errors appear in the main window.".into()} else {"Could not request cache reset.".into()};cx.notify();
            })))
            .child(heading("Thumbnails"))
            .child(note("A separate thumbnail cache serves every supported file, including images below 10 megapixels. Visible thumbnails have priority; the remaining thumbnails are built lazily. This cache is independent of the preview cache reset."))
            .child(self.integration_controls(cx)))
            .child(div().flex_none().p_4().border_t_1().border_color(rgb(0x353538))
            .child(div().flex().items_center().gap_3()
                .child(Button::new("settings-apply").label("Apply").on_click(cx.listener(|this,_,_,cx|this.apply(cx))))
                .child(Button::new("settings-close").label("Close").ghost().on_click(|_,window,_|window.remove_window())))
            .child(div().mt_3().text_xs().child(self.message.clone())))
    }
}
fn heading(text: &'static str) -> AnyElement {
    div()
        .mt_4()
        .mb_3()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
        .into_any_element()
}
fn note(text: &'static str) -> AnyElement {
    div()
        .mt_2()
        .mb_3()
        .text_xs()
        .text_color(rgb(0xb4b4ba))
        .child(text)
        .into_any_element()
}
