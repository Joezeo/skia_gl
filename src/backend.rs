use gl::types::GLint;
use glutin::{
    config::{Config, GlConfig},
    context::{NotCurrentContext, NotCurrentGlContext, PossiblyCurrentContext},
    display::{GetGlDisplay, GlDisplay},
    surface::{GlSurface, Surface, SwapInterval, WindowSurface},
};
use skia_safe::{
    gpu::{gl::FramebufferInfo, BackendRenderTarget, DirectContext, SurfaceOrigin},
    Canvas, Color, ColorType,
};
use std::{
    ffi::CString,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};
use winit::window::Window;

#[cfg(feature = "independent_ui")]
use std::{
    sync::mpsc::{channel, Receiver, Sender},
    thread,
};

use crate::{renderer, SkiaSurface};

pub struct GlCtx {
    not_current_context: Option<NotCurrentContext>,
    possibly_current_context: Option<PossiblyCurrentContext>,
}
impl GlCtx {
    #[inline]
    pub fn new(not_current_context: NotCurrentContext) -> Self {
        Self {
            not_current_context: Some(not_current_context),
            possibly_current_context: None,
        }
    }

    #[inline]
    pub fn make_current(&mut self, surface: &Surface<WindowSurface>) {
        if let Some(not_current_ctx) = self.not_current_context.take() {
            self.possibly_current_context = Some(not_current_ctx.make_current(surface).unwrap())
        }
    }

    #[inline]
    pub fn possibly_current_context(&self) -> Option<&PossiblyCurrentContext> {
        self.possibly_current_context.as_ref()
    }
}

pub struct GlEnv {
    gl_surface: Surface<WindowSurface>,
    gl_ctx: Mutex<GlCtx>,
    gl_config: Config,
}
unsafe impl Sync for GlEnv {}
unsafe impl Send for GlEnv {}
impl GlEnv {
    #[inline]
    pub fn new(gl_surface: Surface<WindowSurface>, gl_ctx: GlCtx, gl_config: Config) -> Self {
        Self {
            gl_surface,
            gl_ctx: Mutex::new(gl_ctx),
            gl_config,
        }
    }

    #[inline]
    pub fn set_vsync(&self) {
        if let Err(res) = self.gl_surface.set_swap_interval(
            self.gl_ctx
                .lock()
                .unwrap()
                .possibly_current_context()
                .unwrap(),
            SwapInterval::Wait(NonZeroU32::new(1).unwrap()),
        ) {
            eprintln!("Error setting vsync: {res:?}");
        }
    }

    #[inline]
    pub fn make_current(&self) {
        self.gl_ctx.lock().unwrap().make_current(&self.gl_surface)
    }

    #[inline]
    pub fn load(&self) {
        gl::load_with(|s| {
            self.gl_config
                .display()
                .get_proc_address(CString::new(s).unwrap().as_c_str())
        });
    }

    #[inline]
    pub fn resize(&self, size: (u32, u32)) {
        if let Some(ctx) = self.gl_ctx.lock().unwrap().possibly_current_context() {
            self.gl_surface.resize(
                ctx,
                NonZeroU32::new(size.0.max(1)).unwrap(),
                NonZeroU32::new(size.1.max(1)).unwrap(),
            )
        }
    }

    #[inline]
    pub fn swap_buffers(&self) {
        if let Some(ctx) = self.gl_ctx.lock().unwrap().possibly_current_context() {
            self.gl_surface.swap_buffers(ctx).unwrap()
        }
    }
}

pub struct SkiaEnv {
    gr_context: DirectContext,
    fb_info: FramebufferInfo,
    surface: SkiaSurface,
}
impl SkiaEnv {
    pub fn canvas(&mut self) -> &mut Canvas {
        self.surface.canvas()
    }

    pub fn resize(&mut self, size: (i32, i32), config: &Config) {
        let num_samples = config.num_samples() as usize;
        let stencil_size = config.num_samples() as usize;

        self.surface = create_surface(
            size,
            self.fb_info,
            &mut self.gr_context,
            num_samples,
            stencil_size,
        );
    }
}

pub struct Backend {
    window: Option<Arc<Window>>,

    #[cfg(not(feature = "independent_ui"))]
    gl_env: Arc<GlEnv>,
    #[cfg(not(feature = "independent_ui"))]
    skia_env: SkiaEnv,

    #[cfg(feature = "independent_ui")]
    sender: Sender<Message>,
}

impl Backend {
    pub fn new(window: Arc<Window>, gl_env: Arc<GlEnv>) -> Self {
        #[cfg(not(feature = "independent_ui"))]
        {
            gl_env.make_current();
            gl_env.load();

            let size = window.inner_size();
            let size = (
                size.width.try_into().expect("Could not convert width"),
                size.height.try_into().expect("Could not convert height"),
            );
            let skia_env = create_skia_env(size, &gl_env.gl_config);
            Self {
                window: Some(window),
                gl_env,
                skia_env,
            }
        }

        #[cfg(feature = "independent_ui")]
        {
            let size = window.inner_size();
            let size = (
                size.width.try_into().expect("Could not convert width"),
                size.height.try_into().expect("Could not convert height"),
            );
            let (sender, receiver) = channel();

            thread::Builder::new()
                .spawn(move || ui_runtime(size, receiver, gl_env))
                .unwrap();

            Self {
                window: Some(window),
                sender,
            }
        }
    }

    #[inline]
    pub fn exit(&mut self) {
        self.window.take();
    }

    #[inline]
    pub fn request_redraw(&self) {
        #[cfg(not(feature = "independent_ui"))]
        if let Some(ref window) = self.window {
            window.request_redraw();
        }
    }

    pub fn notify_resize(&mut self, size: (u32, u32)) {
        #[cfg(not(feature = "independent_ui"))]
        {
            self.skia_env
                .resize((size.0 as i32, size.1 as i32), &self.gl_env.gl_config);
            self.gl_env.resize((size.0 as u32, size.1 as u32));
        }
        #[cfg(feature = "independent_ui")]
        {
            self.sender
                .send(Message::Resize(size.0, size.1))
                .expect("Send resize message failed.")
        }
    }

    #[allow(unused_variables)]
    pub fn render(&mut self, frame: usize) {
        #[cfg(not(feature = "independent_ui"))]
        {
            let canvas = self.skia_env.canvas();
            canvas.clear(Color::WHITE);
            renderer::render_frame(frame % 360, 12, 60, canvas);
            self.skia_env.gr_context.flush_and_submit();
            self.gl_env.swap_buffers();
        }
        #[cfg(feature = "independent_ui")]
        {}
    }
}

fn create_skia_env(size: (i32, i32), gl_config: &Config) -> SkiaEnv {
    let interface = skia_safe::gpu::gl::Interface::new_load_with(|name| {
        if name == "eglGetCurrentDisplay" {
            return std::ptr::null();
        }
        gl_config
            .display()
            .get_proc_address(CString::new(name).unwrap().as_c_str())
    })
    .expect("Could not create interface");

    let mut gr_context = skia_safe::gpu::DirectContext::new_gl(interface, None)
        .expect("Could not create direct context");

    let fb_info = {
        let mut fboid: GLint = 0;
        unsafe { gl::GetIntegerv(gl::FRAMEBUFFER_BINDING, &mut fboid) };

        FramebufferInfo {
            fboid: fboid.try_into().unwrap(),
            format: skia_safe::gpu::gl::Format::RGBA8.into(),
            ..Default::default()
        }
    };

    let num_samples = gl_config.num_samples() as usize;
    let stencil_size = gl_config.stencil_size() as usize;

    let surface = create_surface(size, fb_info, &mut gr_context, num_samples, stencil_size);

    SkiaEnv {
        gr_context,
        fb_info,
        surface,
    }
}

fn create_surface(
    size: (i32, i32),
    fb_info: FramebufferInfo,
    gr_context: &mut skia_safe::gpu::DirectContext,
    num_samples: usize,
    stencil_size: usize,
) -> SkiaSurface {
    let backend_render_target =
        BackendRenderTarget::new_gl(size, Some(num_samples), stencil_size, fb_info);

    SkiaSurface::from_backend_render_target(
        gr_context,
        &backend_render_target,
        SurfaceOrigin::BottomLeft,
        ColorType::RGBA8888,
        None,
        None,
    )
    .expect("Could not create skia surface")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    Resize(u32, u32),
}

#[cfg(feature = "independent_ui")]
pub fn ui_runtime(mut size: (i32, i32), receiver: Receiver<Message>, gl_env: Arc<GlEnv>) {
    use std::time::{Duration, Instant};

    gl_env.make_current();
    gl_env.load();
    gl_env.set_vsync();

    let mut skia_env = create_skia_env(size, &gl_env.gl_config);

    let mut frame = 0usize;
    let mut resized = false;

    let mut previous_frame_start = Instant::now();

    loop {
        let frame_start = Instant::now();

        if let Ok(msg) = receiver.try_recv() {
            match msg {
                Message::Resize(width, height) => {
                    size = (width as i32, height as i32);
                    resized = true;
                }
            }
        }

        let expected_frame_length_seconds = 1.0 / 20.0;
        let frame_duration = Duration::from_secs_f32(expected_frame_length_seconds);

        if frame_start - previous_frame_start > frame_duration {
            if resized {
                gl_env.resize((size.0 as u32, size.1 as u32));
                skia_env.resize((size.0, size.1), &gl_env.gl_config);
            }

            let canvas = skia_env.canvas();
            canvas.clear(Color::WHITE);

            // use skia_safe::{ClipOp, Paint, Rect};
            // canvas.save();
            // let rect = Rect::new(100., 100., 200., 200.);
            // canvas.clip_rect(rect, ClipOp::Difference, false);

            // let rect = Rect::new(0., 0., size.0 as f32, size.1 as f32);
            // let mut paint = Paint::default();
            // paint.set_color(Color::GRAY);
            // canvas.draw_rect(rect, &paint);
            // canvas.restore();

            renderer::render_frame(frame % 360, 12, 60, canvas);
            // std::thread::sleep(std::time::Duration::from_millis(100));

            skia_env.surface.flush_and_submit();
            gl_env.swap_buffers();

            previous_frame_start = frame_start;
            frame += 1;
            resized = false;
        }
    }
}
