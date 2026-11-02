use gpui_kit::{prelude::*, *};

const AUTHOR: &str = "Alexey Kupershtokh";
const AUTHOR_EMAIL: &str = "alexey.kupershtokh@gmail.com";
const REPOSITORY_URL: &str = "https://github.com/AlexeyKupershtokh/Scrutivue";

pub fn open(cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(620.), px(760.)), cx);
    let (handle, _) = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(500.), px(500.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Scrutivue — Help".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
        cx,
        |window, cx| {
            crate::platform::style_window(window);
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                Help { focus }
            })
        },
    )?;
    Ok(handle)
}
struct Help {
    focus: FocusHandle,
}
impl Render for Help {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().id("help-content").size_full().overflow_y_scroll().bg(rgb(0x232325)).text_color(rgb(0xe5e5e7)).p_6().text_sm().font_family("Segoe UI").track_focus(&self.focus)
            .on_key_down(cx.listener(|_,event:&KeyDownEvent,window,_|{if event.keystroke.key=="escape"{window.remove_window();}}))
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).mb_5().child("Help"))
            .child(heading("Adding and selecting images"))
            .child(paragraph("The add buttons stay above the image library. Include subfolders enables recursive scanning, including folders dragged from a file manager. Nested directory symlinks are not followed."))
            .child(paragraph("Startup uses Single mode and normally selects only the first added file as A, even when adding many files or batches. Opening through the registered Windows association selects the requested image instead and lists the other supported images in its folder, without scanning subfolders. A plain click selects that file as the sole A; clicking A again keeps it selected. Ctrl-click another file to add or replace B while keeping A, or Ctrl-click B to remove it. Ctrl-click A leaves A unchanged. The context menu can open files, choose an application, open the containing folder, copy a file name or full path, and explicitly assign A/B. Rows show image dimensions and readable file sizes, such as 7.1 MB or 500 KB."))
            .child(paragraph("Drag rows or use the ↑/↓ buttons at the bottom to reorder them. Clear list cancels background jobs and removes files from the application, not from disk. Neighbor prefetch is off by default; enable Prefetch in Settings to read neighboring frames after the selected images are ready. Statistics takes priority over neighbor prefetch."))
            .child(paragraph("Collapse the library with ‹ or L; reopen it with the separate ☰ button on the left. Refresh synchronizes added folders, adding new files and removing missing ones from the list. Source files are never changed. Controls float over the image; there is no right sidebar."))
            .child(heading("View modes"))
            .child(row("Diff","Absolute difference |A − B|. Choose 1/4/16/64/256× gain beside Diff. Black means no difference."))
            .child(row("Slider","Drag the divider. Both images are already prepared; only their visible areas change."))
            .child(row("Single","Hold the left mouse button or Space to show B; release to return to A. Left-drag pans while showing B; middle-drag pans without switching."))
            .child(paragraph("Slider and Diff require B and are disabled with only A; their keyboard shortcuts are ignored too. Removing B temporarily falls back to Single, then restores the last requested mode when B returns. Explicitly choosing Single replaces that remembered choice. Startup Single does not erase a saved Slider or Diff preference."))
            .child(heading("Mouse and keyboard"))
            .children([("Mouse wheel","Zoom at the cursor, including in the Navigator"),("Drag","Pan; the middle button works in every mode"),("1 / 2 / 3","Single / Slider / Diff (B required for 2 / 3)"),("← / →","Previous / next file: replace sole A, or B while keeping reference A"),("Home / End","First / last file, using the same A/B rule as arrows"),("F / 0","Fit to screen"),("8","800% zoom"),("L","Show / hide the image library"),("C / S","Color picker / Statistics"),("Ctrl+O","Add images"),("Ctrl+Shift+O","Add folder"),("F1","Open help"),("F11","Toggle fullscreen"),("Esc","End the current gesture / close this help window")].map(|(k,v)|row(k,v)))
            .child(heading("Zoom, dimensions and filtering"))
            .child(paragraph("100% means one image pixel per physical screen pixel. Zoom ranges from half of Fit to 800%; Fit itself never exceeds 800%. Above 200%, pixel interiors stay flat; only a narrow one-screen-pixel boundary is smoothed for steadier panning. If image sizes differ, B is scaled to A for viewing only."))
            .child(paragraph("You can pan at every zoom, including Fit and reduced views, to move image details out from under floating panels. The centre of the viewing area always stays inside the image; empty space around the edges is allowed. This also applies after changing images, settings or window size. F / 0 recentres the image and fits it to the screen. Gestures transform the prepared texture; a refinement render is requested after 10 ms without movement."))
            .child(paragraph("The bottom-right Navigator shows reference A with a rectangle for the visible region. Click outside the rectangle to recenter, or drag it to pan. Its wheel zooms around the image point under the cursor without first recentering; wheel events do not also zoom the main view. The same camera limits apply as in the main image."))
            .child(paragraph("High pass filter is at the top: I − Gσ(I). Radius σ is measured in A pixels, with the kernel limited to ±3σ. A zero residual appears as middle grey in normal modes and black in Diff. Changing the radius requires computation; moving the divider does not."))
            .child(paragraph("Diff computes absolute differences before downsampling. A cached pyramid of averaged differences with smooth level transitions reduces moiré; gain is applied after filtering. This does not align images or remove genuine differences."))
            .child(heading("Color picker · C"))
            .child(paragraph("Enable Color picker with the top button or C. Its loupe follows the mouse at 1600% of the original pixels, without smoothing and independently of viewer zoom. Wheel zoom keeps the picker visible and refreshes its sample using the current camera. Diff / High pass / gain are computed locally in the background. The large joined swatch shows A on the left and B on the right, with no line between the colors. Swatches and numeric colors use original RGB16 values, without these effects. Formats include HEX, RGB8/16, HSL, HSV, OKLCH, Lab/XYZ D65 and linear RGB. B coordinates are remapped if dimensions differ. These are developed sRGB colors, not Bayer samples or ICC-managed values."))
            .child(heading("Statistics · S"))
            .child(paragraph("Statistics or S opens a floating card. A background worker publishes results as they become available, independently of mode, zoom, gain and High pass. An LFU cache stores 100 pairs, including partial results; returning to a pair computes only missing stages. Another LFU cache holds 20 resized copies. A/B noise values are statistical estimates, not an exact separation of noise from detail."))
            .child(paragraph("PSNR, MAE, RMSE, maximum error, B−A bias and the fraction of different pixels use full-resolution RGB16 data. Luma SSIM/MS-SSIM, GMSD and SSIMULACRA2 use copies with a long edge up to 512/1536 px; the card shows the actual size. These scores do not replace full-resolution noise analysis. MS-SSIM requires at least 176 px on each axis; unavailable values appear as —. Statistics never stretch mismatched images automatically."))
            .child(paragraph("LPIPS-Squeeze v0.1 uses real pretrained ONNX weights from models/lpips-squeeze-v0.1.onnx beside the EXE. ONNX Runtime (ort) runs it on the CPU using a copy up to 768 px. No Python or startup download is required. Keep the models folder and onnxruntime.dll beside the EXE. ↑ means higher is better; ↓ means lower is better. Metrics cannot establish which image is artistically better."))
            .child(heading("NEF preview"))
            .child(paragraph("Supported Nikon HE NEFs show centre-first sampled bands in Single and Slider while the full RGB16 frame finishes. Global RAW decoding, normalization and white balance still happen first; this is not viewport-only decoding. Previews are display-only. Diff, High pass, statistics and exact pixel inspection wait for complete frames. Other RAW profiles keep the full-frame fallback. Source images and final RGB16 samples are unchanged."))
            .child(heading("DNG and settings"))
            .child(paragraph("JPEG XL DNG 1.7 (compression 52546, unsigned RGB/grey16) is decompressed in memory and developed by the same LibRaw, preserving camera calibration. Thumbnails come separately from the embedded JPEG preview. Source files are not modified."))
            .child(paragraph("Mode, gain, High pass/radius, zoom/center, divider, recursion, window size and library/Statistics/Color picker visibility are saved in settings.json in the user configuration directory. Writes run in the background with validation and atomic replacement; the file list is not restored. Use --settings-file for a separate profile. Corrupt or newer settings files are not overwritten."))
            .child(heading("Settings and caches"))
            .child(paragraph("Settings opens a separate window. The configurable RAM cache is split equally between complete RGB16 frames and decoded display previews. Active A/B frames, decoder temporaries, analysis and GPU buffers are additional working memory. The default total RAM cache is 2048 MiB. Changes apply without restarting and are saved with the other preferences."))
            .child(paragraph("The disk preview cache defaults to 1024 MiB under the temporary directory. Choose another parent location or set its size to 0 to disable disk storage. Images of at least 10 megapixels produce a 2560-pixel long-edge JPEG XL preview, distance 0.05 and effort 3, in an application-owned SHA-256 hash tree. Previews are display-only: Diff, High pass, Statistics and Color picker still use complete source frames. Clear disk preview cache removes only application cache files, never source images."))
            .child(paragraph("Previews are built lazily when viewing or prefetching a file. Build previews for all added files extends idle background work to the whole library, with selected images taking priority. Thumbnails use an independent lazy cache for all supported files, including images below 10 megapixels; visible rows have priority. Resetting preview storage does not remove thumbnails."))
            .child(heading("Windows Open with"))
            .child(paragraph("In Settings, scroll to Windows Open with and click Register to offer this executable for all supported image extensions, or Remove to remove its owned entries. These actions take effect immediately, independently of Apply. They are per-user, require no administrator rights and do not change default apps. An association launch selects the requested image and lists its folder's supported images. Register again after moving or updating the portable application to refresh its command. Startup never registers automatically; unrelated registrations and manually changed commands are preserved."))
            .child(heading("Author"))
            .child(paragraph(AUTHOR))
            .child(paragraph(AUTHOR_EMAIL))
            .child(div().id("help-repository").mb_3().text_color(rgb(0xb4b4ba)).underline().cursor_pointer()
                .on_click(|_, _, cx| cx.open_url(REPOSITORY_URL))
                .child(REPOSITORY_URL))
    }
}
fn heading(text: &'static str) -> AnyElement {
    div()
        .mt_4()
        .mb_2()
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
        .into_any_element()
}
fn paragraph(text: &'static str) -> AnyElement {
    div()
        .mb_3()
        .text_color(rgb(0xb4b4ba))
        .child(text)
        .into_any_element()
}
fn row(key: &'static str, text: &'static str) -> AnyElement {
    div()
        .w_full()
        .mb_2()
        .flex()
        .gap_4()
        .child(div().w(px(145.)).flex_none().child(key))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(rgb(0xb4b4ba))
                .child(text),
        )
        .into_any_element()
}
