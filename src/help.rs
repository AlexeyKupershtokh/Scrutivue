use gpui_kit::{prelude::*, *};

pub fn open(cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(620.), px(760.)), cx);
    let (handle, _) = gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(500.), px(500.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Lumen Diff — справка".into()),
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
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).mb_5().child("Справка"))
            .child(heading("Добавление и выбор"))
            .child(paragraph("Кнопки добавления закреплены над списком файлов. «Включая подпапки» включает рекурсивный обход папок, в том числе перетащенных из проводника. Вложенные символические ссылки на каталоги не обходятся."))
            .child(paragraph("Первый щелчок по файлу назначает A, второй — B. Следующий новый файл заменяет B, сохраняя A. Повторный щелчок снимает выбор; если убрать A, оставшийся кадр становится A. Метки A/B показаны в списке."))
            .child(paragraph("Строки можно перетаскивать или перемещать кнопками ↑/↓ внизу панели. «Очистить список» отменяет фоновые задания и убирает файлы из приложения, не удаляя их с диска."))
            .child(heading("Режимы просмотра"))
            .child(row("Single","Один кадр; кнопки A/B выбирают показываемое изображение."))
            .child(row("Diff","Абсолютная разность |A − B|. Усиление 1/4/16/64/256× находится рядом с Diff. Чёрный означает отсутствие разницы."))
            .child(row("Both slider","Перетаскивайте разделитель. Оба изображения уже готовы; меняется только их видимая область."))
            .child(row("Both click","Удерживайте левую кнопку мыши или пробел для показа B; отпустите для возврата к A."))
            .child(heading("Мышь и клавиатура"))
            .children([("Колесо","Зум у курсора"),("Перетаскивание","Панорамирование; средняя кнопка работает во всех режимах"),("1 / 2 / 3 / 4","Single / Diff / Slider / Click"),("← / →","Предыдущий / следующий файл вместо последнего выбранного"),("Home / End","Первый / последний файл"),("F / 0","Fit to screen"),("Ctrl+O","Добавить изображения"),("Ctrl+Shift+O","Добавить папку"),("F1","Открыть справку"),("Esc","Завершить жест / закрыть это окно")].map(|(k,v)|row(k,v)))
            .child(heading("Зум, размеры и шум"))
            .child(paragraph("100% означает один пиксель изображения на физический пиксель экрана. Позиция и зум сохраняются при смене кадров. При разном разрешении B масштабируется до размера A; автоматического совмещения сцены нет."))
            .child(paragraph("Вычитание Gaussian: I − Gσ(I). Радиус σ задаётся в пикселях A, ядро ограничено ±3σ. В обычных режимах ноль остатка показан средним серым; в Diff — чёрным. Изменение радиуса требует вычисления, движение разделителя — нет."))
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
