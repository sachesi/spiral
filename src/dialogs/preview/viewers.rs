//! What the preview shows a file in: video and sound, PDF pages, a picture to zoom and pan,
//! text, and a page of facts for anything else.

use super::*;

pub(super) fn page_text(page: u32, pages: u32) -> String {
    // Translators: %p is the page being shown, %n the number of pages in the document.
    gettext("%p of %n")
        .replace("%p", &page.to_string())
        .replace("%n", &pages.to_string())
}

/// A picture that fills the dialog, which is shaped like it: a small one is enlarged
/// rather than framed by empty space.
pub(super) fn picture(paintable: &impl IsA<gdk::Paintable>) -> gtk::Picture {
    gtk::Picture::builder()
        .paintable(paintable)
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .hexpand(true)
        .vexpand(true)
        .build()
}

/// The scale a fitted picture is drawn at: where zooming starts and where zooming out
/// ends. Above 1 for a picture smaller than the dialog, which fitting enlarges.
pub(super) fn fit_scale(picture: &gtk::Picture, scroll: &gtk::ScrolledWindow) -> f64 {
    let Some(paintable) = picture.paintable() else {
        return 1.0;
    };
    let (width, height) = (
        paintable.intrinsic_width() as f64,
        paintable.intrinsic_height() as f64,
    );
    if width <= 0.0 || height <= 0.0 {
        return 1.0;
    }
    (scroll.width() as f64 / width).min(scroll.height() as f64 / height)
}

/// Whether a PDF page can be drawn at all: without the tool, a PDF is shaped like the icon
/// it will end up showing instead of shrinking into it once the attempt has failed.
pub(super) fn can_render_pdf() -> bool {
    glib::find_program_in_path("pdftoppm").is_some()
}

/// The hand that says a zoomed picture can be dragged, and nothing while it fits.
pub(super) fn pan_cursor(level: f64) -> Option<&'static str> {
    (level > 0.0).then_some("grab")
}

pub(super) fn flat_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button
}

/// Text is drawn by GtkSourceView, which colours whatever language it recognises the file
/// as and leaves anything else plain. Lines are not wrapped: source is read as it is
/// written, and the preview scrolls sideways for the long ones.
pub(super) fn text_view(text: &str, info: &gio::FileInfo, content_type: &str) -> gtk::Widget {
    use sourceview5::prelude::*;

    static SOURCE_INIT: std::sync::Once = std::sync::Once::new();
    SOURCE_INIT.call_once(sourceview5::init);

    let buffer = sourceview5::Buffer::new(None);
    buffer.set_language(
        sourceview5::LanguageManager::default()
            .guess_language(Some(info.display_name().as_str()), Some(content_type))
            .as_ref(),
    );
    // One of the schemes GtkSourceView ships; with none set the
    // language is recognised but nothing is coloured.
    let scheme = if adw::StyleManager::default().is_dark() {
        "Adwaita-dark"
    } else {
        "Adwaita"
    };
    buffer.set_style_scheme(
        sourceview5::StyleSchemeManager::default()
            .scheme(scheme)
            .as_ref(),
    );
    buffer.set_text(text);
    let view = sourceview5::View::with_buffer(&buffer);
    view.add_css_class("spiral-source-view");
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    // Numbers down the side: a preview of a file is read to find something in it, and the
    // line it is on is what gets said out loud afterwards.
    view.set_show_line_numbers(true);
    view.set_top_margin(12);
    view.set_bottom_margin(12);
    view.set_left_margin(12);
    view.set_right_margin(12);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .hexpand(true)
        .vexpand(true)
        .build();
    // No scrollbars anywhere in the preview; the wheel and the keys still scroll.
    scroll.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
    scroll.upcast()
}

impl PreviewDialog {
    /// The one player, pointed at `file`, with this dialog listening to it until the next
    /// file or the close takes it away again. `None` where there is no playback to offer.
    pub(super) fn player(
        &self,
        info: &gio::FileInfo,
        file: &gio::File,
    ) -> Option<crate::player::Player> {
        let player = crate::player::player()?;
        player.set_file(Some(file.clone()));
        let generation = self.imp().generation.get();
        let info = info.clone();
        let failed = player.connect_error_notify(glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            move |player| {
                // A file the pipeline cannot play shows what it would have shown anyway,
                // in the shape the dialog already has.
                if player.error().is_some() && dialog.imp().generation.get() == generation {
                    dialog.show_child(&dialog.info_page(&info), true);
                }
            }
        ));
        self.imp().player_handlers.borrow_mut().push(failed);
        Some(player)
    }

    /// Video fills the room there is in its own proportions, whatever its pixel count: a
    /// small clip is worth a window one can watch. The stream only confirms the shape the
    /// thumbnail or the container gave, or corrects the one most video has.
    ///
    /// The page goes on screen with its first frame, over the thumbnail or the spinner
    /// holding its place, and not before: a video with nothing to draw is a black box.
    pub(super) fn video(&self, info: &gio::FileInfo, file: &gio::File) -> Option<gtk::Widget> {
        let Some(player) = self.player(info, file) else {
            return Some(self.info_page(info));
        };
        let video = gtk::Video::for_media_stream(Some(&player));
        video.set_autoplay(true);
        let generation = self.imp().generation.get();
        // The size comes with the first frame, which may be after the stream is prepared.
        // Strong, because nothing else holds the page until it is shown; the handlers go
        // with the next file or the close.
        let reshape = glib::clone!(
            #[weak(rename_to = dialog)]
            self,
            #[strong]
            video,
            move |player: &crate::player::Player| {
                if !player.is_prepared() || dialog.imp().generation.get() != generation {
                    return;
                }
                let (w, h) = (player.intrinsic_width(), player.intrinsic_height());
                glib::g_debug!("spiral", "preview: the stream reports {w}x{h}");
                if w > 0 && h > 0 {
                    dialog.shape_filled(w as f64, h as f64);
                }
                if video.parent().is_none() && (w > 0 && h > 0 || !player.has_video()) {
                    dialog.show_child(&video, true);
                }
            }
        );
        let handlers = [
            player.connect_prepared_notify(glib::clone!(
                #[strong]
                reshape,
                move |player| reshape(player)
            )),
            player.connect_invalidate_size(glib::clone!(
                #[strong]
                reshape,
                move |player| reshape(player)
            )),
        ];
        self.imp().player_handlers.borrow_mut().extend(handlers);
        None
    }

    /// Sound has its icon to draw, and the cover in its place once the file has given one
    /// up; either way the transport controls sit under it.
    pub(super) fn sound(&self, info: &gio::FileInfo, file: &gio::File) -> gtk::Widget {
        let Some(player) = self.player(info, file) else {
            return self.info_page(info);
        };
        player.play();
        let controls = gtk::MediaControls::builder()
            .media_stream(&player)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .build();
        let art = gtk::Image::from_gicon(&file_utils::icon_of(info));
        art.set_pixel_size(SOUND_ICON_SIZE);
        // The rounded corners of the cover come from clipping the widget, so the cover is
        // made to fill it exactly: square, at the size of the icon it replaces, in a widget
        // no wider than that rather than one stretched across the player.
        art.set_halign(gtk::Align::Center);
        art.set_overflow(gtk::Overflow::Hidden);
        let side = SOUND_ICON_SIZE * self.scale_factor();
        let cover = player.connect_cover(glib::clone!(
            #[weak]
            art,
            move |player| {
                let Some(bytes) = player.cover() else { return };
                glib::spawn_future_local(glib::clone!(
                    #[weak]
                    art,
                    async move {
                        if let Some(texture) = cover_texture(bytes, side).await {
                            art.set_paintable(Some(&texture));
                            art.add_css_class("spiral-preview-cover");
                        }
                    }
                ));
            }
        ));
        self.imp().player_handlers.borrow_mut().push(cover);
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .build();
        column.append(&art);
        column.append(&controls);
        column.upcast()
    }

    /// One PDF page with the buttons that turn it, or nothing when `pdftoppm` is missing.
    pub(super) async fn pdf(&self, path: PathBuf) -> Option<gtk::Widget> {
        let known = self.imp().page_count.get().zip(self.imp().page_size.get());
        let (pages, size) = match known {
            Some(facts) => facts,
            None => pdf_info(path.clone()).await?,
        };
        // The shape was decided before the dialog opened, from the thumbnail or from the
        // file itself; the page dictionary of a modern PDF is compressed and neither may
        // have found it. Rather than resize a window the reader is already looking at, the
        // page is drawn inside the shape there is, and only a page lying on its side —
        // which no margin can absorb — is worth moving the window for.
        let (shaped_width, shaped_height) = self.imp().shaped.get();
        let shaped = shaped_width as f64 / shaped_height as f64;
        if ((size.0 / size.1) / shaped - 1.0).abs() > SHAPE_SLACK {
            self.shape_filled(size.0, size.1);
        }
        let first = pdf_page(path.clone(), 1).await?;
        let page = Rc::new(Cell::new(1u32));
        let picture = picture(&first);
        let label = gtk::Label::new(Some(&page_text(1, pages)));
        let previous = flat_button("go-previous-symbolic", &gettext("Previous Page"));
        previous.set_sensitive(false);
        let next = flat_button("go-next-symbolic", &gettext("Next Page"));
        next.set_sensitive(pages > 1);

        // Weak, because the buttons hold this closure.
        let flip = glib::clone!(
            #[strong]
            page,
            #[weak]
            picture,
            #[weak]
            label,
            #[weak]
            previous,
            #[weak]
            next,
            move |delta: i32| {
                let target = (page.get() as i32 + delta).clamp(1, pages as i32) as u32;
                if target == page.get() {
                    return;
                }
                page.set(target);
                label.set_label(&page_text(target, pages));
                previous.set_sensitive(target > 1);
                next.set_sensitive(target < pages);
                glib::spawn_future_local(glib::clone!(
                    #[strong]
                    path,
                    #[weak]
                    picture,
                    #[strong]
                    page,
                    async move {
                        // A page rendered after the reader moved on is dropped.
                        if let Some(texture) = pdf_page(path, target).await
                            && page.get() == target
                        {
                            picture.set_paintable(Some(&texture));
                        }
                    }
                ));
            }
        );
        previous.connect_clicked(glib::clone!(
            #[strong]
            flip,
            move |_| flip(-1)
        ));
        next.connect_clicked(glib::clone!(
            #[strong]
            flip,
            move |_| flip(1)
        ));
        self.imp().flip.replace(Some(Box::new(flip)));
        let extras: Vec<gtk::Widget> = vec![
            previous.upcast(),
            label.upcast(),
            next.upcast(),
            gtk::Separator::new(gtk::Orientation::Vertical).upcast(),
        ];
        Some(self.zoomable(&picture, &extras))
    }

    /// A picture that fits the dialog until the buttons, Ctrl with + and -, or Ctrl and
    /// the wheel say otherwise. `extras` share the floating bar, for the PDF page buttons.
    pub(super) fn zoomable(&self, picture: &gtk::Picture, extras: &[gtk::Widget]) -> gtk::Widget {
        let scroll = gtk::ScrolledWindow::builder()
            .child(picture)
            .hexpand(true)
            .vexpand(true)
            .build();
        // Scrolling off while the picture is fitted: the policy is what makes the viewport
        // hold the picture to its own size instead of the picture's natural one.
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
        let level = Rc::new(Cell::new(0.0f64));
        let label = gtk::Label::new(Some(&gettext("Fit")));
        label.set_width_chars(5);
        let out = flat_button("zoom-out-symbolic", &gettext("Zoom Out"));
        let fit = flat_button("zoom-fit-best-symbolic", &gettext("Fit to Window"));
        let in_ = flat_button("zoom-in-symbolic", &gettext("Zoom In"));

        // Where the pointer is, so that the wheel zooms around what is under it.
        let pointer = Rc::new(Cell::new(None::<(f64, f64)>));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[strong]
            pointer,
            move |_, x, y| pointer.set(Some((x, y)))
        ));
        motion.connect_leave(glib::clone!(
            #[strong]
            pointer,
            move |_| pointer.set(None)
        ));
        scroll.add_controller(motion);

        // Weak, because the wheel and the drag on `scroll` hold this closure: a strong
        // reference from there would keep the widget alive after the page is gone.
        let zoom = glib::clone!(
            #[strong]
            level,
            #[weak]
            picture,
            #[weak]
            scroll,
            #[weak]
            label,
            move |step: f64, at: Option<(f64, f64)>| {
                let fit = fit_scale(&picture, &scroll);
                let before = if level.get() > 0.0 { level.get() } else { fit };
                let next = if step <= 0.0 {
                    0.0
                } else {
                    let wanted = (before * step).clamp(ZOOM_MIN, ZOOM_MAX * fit.max(1.0));
                    // Zooming out stops at the fit instead of counting below it, where the
                    // picture cannot follow the number any further.
                    if wanted <= fit { 0.0 } else { wanted }
                };
                level.set(next);
                let Some(paintable) = picture.paintable() else {
                    return;
                };
                scroll.set_cursor_from_name(pan_cursor(next));
                if next <= 0.0 {
                    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
                    picture.set_content_fit(gtk::ContentFit::Contain);
                    picture.set_size_request(-1, -1);
                    label.set_label(&gettext("Fit"));
                    return;
                }
                picture.set_content_fit(gtk::ContentFit::Contain);
                scroll.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
                let (width, height) = (
                    paintable.intrinsic_width() as f64 * next,
                    paintable.intrinsic_height() as f64 * next,
                );
                picture.set_size_request(width as i32, height as i32);
                label.set_label(&format!("{}%", (next * 100.0).round()));
                // Hold the point the zoom happened around still. The scrolled window only
                // learns the new size in the next layout, so the room for it is made here
                // and the offsets land in the same frame as the picture that needs them.
                let (anchor_x, anchor_y) =
                    at.unwrap_or((scroll.width() as f64 / 2.0, scroll.height() as f64 / 2.0));
                let ratio = next / before;
                let (horizontal, vertical) = (scroll.hadjustment(), scroll.vadjustment());
                horizontal.set_upper(width.max(scroll.width() as f64));
                vertical.set_upper(height.max(scroll.height() as f64));
                horizontal.set_value((horizontal.value() + anchor_x) * ratio - anchor_x);
                vertical.set_value((vertical.value() + anchor_y) * ratio - anchor_y);
            }
        );
        for (button, step) in [(&out, 1.0 / ZOOM_STEP), (&fit, 0.0), (&in_, ZOOM_STEP)] {
            button.connect_clicked(glib::clone!(
                #[strong]
                zoom,
                move |_| zoom(step, None)
            ));
        }
        // Ctrl and the wheel, as everywhere else that zooms.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        wheel.connect_scroll(glib::clone!(
            #[strong]
            zoom,
            #[strong]
            pointer,
            move |controller, _, dy| {
                if !controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                    || dy == 0.0
                {
                    return glib::Propagation::Proceed;
                }
                // A wheel notch is a whole step; a touchpad sends fractions of one and
                // zooms by fractions of a step, which is what makes it feel continuous.
                zoom(ZOOM_STEP.powf(-dy), pointer.get());
                glib::Propagation::Stop
            }
        ));
        scroll.add_controller(wheel);
        // Zoomed in, the picture is moved by dragging it: there are no scrollbars to grab.
        let drag = gtk::GestureDrag::new();
        let from = Rc::new(Cell::new((0.0, 0.0)));
        drag.connect_drag_begin(glib::clone!(
            #[weak]
            scroll,
            #[strong]
            from,
            move |_, _, _| {
                from.set((scroll.hadjustment().value(), scroll.vadjustment().value()));
                scroll.set_cursor_from_name(Some("grabbing"));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak]
            scroll,
            #[strong]
            from,
            move |_, x, y| {
                let (left, top) = from.get();
                scroll.hadjustment().set_value(left - x);
                scroll.vadjustment().set_value(top - y);
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak]
            scroll,
            #[strong]
            level,
            move |_, _, _| scroll.set_cursor_from_name(pan_cursor(level.get()))
        ));
        scroll.add_controller(drag);
        self.imp().zoom.replace(Some(Box::new(move |delta| {
            let step = match delta {
                1 => ZOOM_STEP,
                -1 => 1.0 / ZOOM_STEP,
                _ => 0.0,
            };
            zoom(step, None)
        })));

        let bar = gtk::Box::builder()
            .spacing(6)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(12)
            .css_classes(["floating-bar"])
            .build();
        for extra in extras {
            bar.append(extra);
        }
        bar.append(&out);
        bar.append(&label);
        bar.append(&in_);
        bar.append(&fit);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scroll));
        overlay.add_overlay(&bar);
        overlay.upcast()
    }

    /// Files nothing can draw: their icon alone, since the header already carries the name
    /// and the type.
    pub(super) fn info_page(&self, info: &gio::FileInfo) -> gtk::Widget {
        let icon = gtk::Image::from_gicon(&file_utils::icon_of(info));
        icon.set_pixel_size(ICON_SIZE);
        icon.set_halign(gtk::Align::Center);
        icon.set_valign(gtk::Align::Center);
        icon.set_vexpand(true);
        icon.upcast()
    }
}
