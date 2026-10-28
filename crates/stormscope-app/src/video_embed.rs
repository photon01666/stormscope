//! Native WebView child embedded in the Stormscope window for video playback.
//!
//! The WebView is a real OS child view (WebView2 on Windows, WebKitGTK on
//! X11 Linux), so it always paints above egui. The egui player window keeps
//! the title/Close/Fullscreen header outside the video rectangle; this module
//! only tracks that rectangle and resizes/loads the WebView.

#![cfg(not(target_os = "android"))]

use crate::state::YouTubePlayer;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WindowHandle,
};
use wry::{Rect, WebViewBuilder};

/// A borrowed raw handle to the eframe window. The main window outlives this
/// struct; storing the raw handle lets the WebView be created lazily when the
/// first video opens.
pub struct ParentWindow {
    raw: RawWindowHandle,
    display: RawDisplayHandle,
}

impl ParentWindow {
    pub fn from_context(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        let raw = cc.window_handle().ok()?.as_raw();
        let display = cc.display_handle().ok()?.as_raw();
        Some(Self { raw, display })
    }
}

impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: the eframe root window is alive for the whole app lifetime
        // and this struct is owned by the app.
        unsafe { Ok(WindowHandle::borrow_raw(self.raw)) }
    }
}

impl HasDisplayHandle for ParentWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: as above.
        unsafe { Ok(DisplayHandle::borrow_raw(self.display)) }
    }
}

pub struct VideoEmbed {
    parent: ParentWindow,
    webview: Option<wry::WebView>,
    video_id: Option<String>,
    last_bounds: Option<egui::Rect>,
    hidden: bool,
    close_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl VideoEmbed {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            // wry's Linux child view uses WebKitGTK even though winit owns the
            // window. Initialise GTK on the UI thread before the first WebView.
            match gtk::init() {
                Ok(()) => GTK_READY.store(true, std::sync::atomic::Ordering::Relaxed),
                Err(error) => log::warn!("GTK init for embedded video failed: {error}"),
            }
        }
        Some(Self {
            parent: ParentWindow::from_context(cc)?,
            webview: None,
            video_id: None,
            last_bounds: None,
            hidden: false,
            close_requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }

    /// True once the page's own Close button has been pressed.
    pub fn take_close_requested(&self) -> bool {
        self.close_requested
            .swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    /// Create/load/resize the WebView to match the egui player's video area.
    /// `None` closes it.
    pub fn sync(
        &mut self,
        player: Option<&YouTubePlayer>,
        rect: Option<egui::Rect>,
        pixels_per_point: f32,
    ) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        pump_gtk();

        let Some(player) = player else {
            self.webview = None;
            self.video_id = None;
            self.last_bounds = None;
            return Ok(());
        };
        let Some(rect) = rect else {
            // The egui window is being dragged/resized: hide the native view
            // for the gesture instead of letting it lag behind the border.
            if let Some(webview) = &self.webview {
                webview
                    .set_visible(false)
                    .map_err(|error| format!("{error}"))?;
                self.hidden = true;
            }
            return Ok(());
        };
        if rect.width() < 32.0 || rect.height() < 32.0 {
            return Ok(());
        }
        let bounds = bounds(rect, pixels_per_point);
        if self.hidden
            && let Some(webview) = &self.webview
        {
            webview
                .set_visible(true)
                .map_err(|error| format!("{error}"))?;
            self.hidden = false;
        }
        if self.webview.is_none() {
            let close_requested = self.close_requested.clone();
            let webview = WebViewBuilder::new()
                .with_url_and_headers(embed_url(&player.id), embed_headers())
                .with_autoplay(true)
                .with_user_agent(user_agent())
                .with_initialization_script(player_close_script())
                .with_ipc_handler(move |request| {
                    if request.body().trim() == "close" {
                        close_requested.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                })
                .with_bounds(bounds)
                .build_as_child(&self.parent)
                .map_err(|error| format!("{error}"))?;
            // Some child-WebView backends ignore the builder's initial bounds
            // and create a default-sized view. Apply them again after creation
            // so the first frame already matches the egui video rectangle.
            webview
                .set_bounds(bounds)
                .map_err(|error| format!("{error}"))?;
            webview
                .set_visible(true)
                .map_err(|error| format!("{error}"))?;
            self.webview = Some(webview);
            self.video_id = Some(player.id.clone());
            self.last_bounds = Some(rect);
            return Ok(());
        }

        if self.video_id.as_deref() != Some(player.id.as_str()) {
            if let Some(webview) = &self.webview {
                webview
                    .load_url_with_headers(&embed_url(&player.id), embed_headers())
                    .map_err(|error| format!("{error}"))?;
            }
            self.video_id = Some(player.id.clone());
        }
        if self.last_bounds != Some(rect)
            && let Some(webview) = &self.webview
        {
            webview
                .set_bounds(bounds)
                .map_err(|error| format!("{error}"))?;
            self.last_bounds = Some(rect);
        }
        Ok(())
    }
}

fn embed_url(video_id: &str) -> String {
    format!(
        "https://www.youtube.com/embed/{video_id}?autoplay=1&playsinline=1&rel=0&modestbranding=1"
    )
}

/// YouTube's embedded player now requires an identifying HTTP Referer. A
/// WebView loading the embed URL directly would otherwise show error 153
/// ("Video player configuration error").
fn embed_headers() -> wry::http::HeaderMap {
    let mut headers = wry::http::HeaderMap::new();
    headers.insert(
        wry::http::header::REFERER,
        wry::http::HeaderValue::from_static("https://github.com/photon01666/stormscope"),
    );
    headers.insert(
        wry::http::header::REFERRER_POLICY,
        wry::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers
}

/// WebView bounds are physical pixels relative to the parent window.
/// egui works in points, so convert with the current pixels-per-point.
fn bounds(rect: egui::Rect, pixels_per_point: f32) -> Rect {
    let ppp = pixels_per_point as f64;
    // The X11 child window uses GTK logical coordinates for position but
    // needs physical pixels for its size (wry's GTK window does not scale the
    // allocated child the way egui scales the parent surface).
    Rect {
        position: wry::dpi::LogicalPosition::new(rect.min.x as f64, rect.min.y as f64).into(),
        size: wry::dpi::PhysicalSize::new(
            (rect.width() as f64 * ppp).round(),
            (rect.height() as f64 * ppp).round(),
        )
        .into(),
    }
}

/// Add a Close button to YouTube's player page itself. The egui title row can
/// sit under the native child on some X11 compositors, so the video window
/// needs its own always-visible Close affordance on the border.
fn player_close_script() -> &'static str {
    r#"
(() => {
  const add = () => {
    if (!document.body) { setTimeout(add, 100); return; }
    if (document.getElementById('stormscope-close')) return;
    const button = document.createElement('button');
    button.id = 'stormscope-close';
    button.type = 'button';
    button.textContent = 'Close';
    button.setAttribute('aria-label', 'Close video');
    button.style.cssText = [
      'position:fixed', 'top:10px', 'right:10px', 'z-index:2147483647',
      'appearance:none', 'border:1px solid rgba(255,255,255,.55)',
      'border-radius:8px', 'padding:8px 14px',
      'background:rgba(20,24,30,.92)', 'color:#fff',
      'font:600 14px system-ui,-apple-system,sans-serif',
      'cursor:pointer', 'box-shadow:0 2px 14px rgba(0,0,0,.65)'
    ].join(';');
    button.addEventListener('mouseenter', () => {
      button.style.background = 'rgba(150,38,38,.95)';
    });
    button.addEventListener('mouseleave', () => {
      button.style.background = 'rgba(20,24,30,.92)';
    });
    button.addEventListener('click', () => {
      if (window.ipc && window.ipc.postMessage) window.ipc.postMessage('close');
    });
    document.body.appendChild(button);
  };
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', add, { once: true });
  } else {
    add();
  }
  setTimeout(add, 1200);
})();
"#
}

fn user_agent() -> &'static str {
    if cfg!(windows) {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    } else {
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    }
}

#[cfg(target_os = "linux")]
static GTK_READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "linux")]
fn pump_gtk() {
    if GTK_READY.load(std::sync::atomic::Ordering::Relaxed) {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
    }
}
