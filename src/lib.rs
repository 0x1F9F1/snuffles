use std::rc::Rc;
use std::ops::Range;
use std::borrow::Cow;
use std::time::Instant;
use wgpu::util::{DeviceExt, BufferInitDescriptor};
use cgmath::{Deg, Rad, Angle, Point3, point3, Vector3, Matrix4};
use winit::dpi::LogicalSize;
use winit::event::{Event, WindowEvent, VirtualKeyCode, ElementState};
use winit::event::MouseButton;
use winit::event::MouseScrollDelta;
use winit::event::DeviceEvent::MouseMotion;
use winit::window::WindowBuilder;
use winit::event_loop::{EventLoop, ControlFlow, EventLoopProxy};

pub use wgpu::*;
pub use winit;
pub use cgmath;

/// Type to use user events to the event loop
type UserEvent = bool;

/// Wrapper type around [`Error`]
type Result<T> = std::result::Result<T, Error>;

/// Should rendering be based on RedrawRequested or MainEventsCleared
/// On Windows, Vsync + RedrawRequested seems to limit the FPS to 60
const USE_REDRAW_REQUEST: bool = false;

/// Error statuses
#[derive(Debug)]
pub enum Error {
    /// Creating the window failed
    CreateWindow(winit::error::OsError),

    /// Failed to get a compatible adapter
    GetAdapter,

    /// Failed to create to the logical device and queue
    CreateDevice(RequestDeviceError),

    /// Failed to get the preferred format for the output device's surface
    PreferredFormat,

    /// Failed to get the texture associated with a surface we are rendering
    /// to
    GetSurfaceTexture(SurfaceError),

    /// Grabbing the cursor failed
    CursorGrab(winit::error::ExternalError),

    /// Failed to send an event with an event loop proxy
    SendEvent(winit::event_loop::EventLoopClosed<UserEvent>),
}

/// Camera modes for the built-in cameras
pub enum CameraMode {
    /// User-controlled camera
    None,

    /// A "flying" 3d camera where 'W' and 'S' move your forwards or backwards
    /// relative to the cameras facing, 'A' and 'S' strafe you left and right
    /// and the scroll wheel increases or decreases this movement speed.
    ///
    /// The camera pitch and yaw can be panned by left clicking and moving the
    /// mouse
    Flight3d,
}

/// Camera states for the built-in cameras
enum CameraState {
    /// User-controlled camera
    None,

    /// A "flying" 3d camera where 'W' and 'S' move your forwards or backwards
    /// relative to the cameras facing, 'A' and 'S' strafe you left and right
    /// and the scroll wheel increases or decreases this movement speed.
    ///
    /// The camera pitch and yaw can be panned by left clicking and moving the
    /// mouse
    Flight3d {
        /// Eye location
        eye: Point3<f32>,

        /// Camera pitch
        pitch: Deg<f32>,

        /// Camera yaw
        yaw: Deg<f32>,

        /// Speed to move the camera at when holding a movement key
        speed: f32,

        /// Tracks if the mouse is currently held (thus panning)
        panning: bool,

        /// Is the W key currently pressed?
        key_w: bool,

        /// Is the A key currently pressed?
        key_a: bool,

        /// Is the S key currently pressed?
        key_s: bool,

        /// Is the D key currently pressed?
        key_d: bool,
    }
}

/// The format for vertices for this program
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Vertex {
    /// Position of the vertex
    pub pos: Vector3<f32>,

    /// Red component of the vertex
    pub r: u8,

    /// Green component of the vertex
    pub g: u8,

    /// Blue component of the vertex
    pub b: u8,

    /// Padding
    _padding: u8,
}

impl Vertex {
    /// Create a new vertex with color
    pub const fn new(x: f32, y: f32, z: f32, r: u8, g: u8, b: u8) -> Self {
        Self { pos: Vector3::new(x, y, z), r, g, b, _padding: 0 }
    }
}

/// Should the drawing persist in the scene buffer such that it can be
/// incrementally built on
#[derive(Clone, Copy)]
pub enum Persist {
    Yes,
    No,
}

/// Different commands we can use to draw data
#[derive(Clone)]
pub enum DrawCommand {
    Triangles(Persist, Rc<Buffer>, std::ops::Range<u32>),
    Lines(Persist, Rc<Buffer>, std::ops::Range<u32>),
}

/// Required trait to handle rendering events
pub trait EventHandler: Sized {
    /// Called when the window is created to create this handler
    fn create(window: &mut Window<Self>) -> Self;

    /// A key was pressed down
    fn key_down(&mut self, _key: VirtualKeyCode) {}

    /// A key was released
    fn key_up(&mut self, _key: VirtualKeyCode) {}

    /// The mouse moved
    fn mouse_move(&mut self, _window: &mut Window<Self>,
        _delta_x: f64, _delta_y: f64) {}

    /// The mouse vertically scrolled
    fn mouse_scroll(&mut self, _window: &mut Window<Self>, _delta_y: f32) {}

    /// A mouse button was pressed
    fn mouse_down(&mut self, _window: &mut Window<Self>,
        _button: winit::event::MouseButton) {}

    /// A mouse button was released
    fn mouse_up(&mut self, _window: &mut Window<Self>,
        _button: winit::event::MouseButton) {}

    /// We're deciding if we want to schedule a new frame for drawing. You
    /// should use this if you need another frame to be drawn without an
    /// event firing (eg. moving while a key is still held down)
    fn should_redraw(&mut self, _window: &mut Window<Self>) {}

    /// Called before rendering each frame to get draw commands
    /// A user should invoke `window.push_command()` to enqueue draw commands
    fn render(&mut self, _window: &mut Window<Self>, incremental: bool);
}

/// MSAA values
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum Msaa {
    /// No subsampling
    None = 1,

    /// 4x MSAA
    X4 = 4,
}

/// Vsync
#[derive(Clone, Copy)]
pub enum Vsync {
    /// Disabled
    Off,

    /// Enabled
    On,
}

/// Proxy to request a redraw from another thread
pub struct RedrawTrigger(EventLoopProxy<UserEvent>);

impl RedrawTrigger {
    /// Request a redraw
    ///
    /// If `incremental` is set, initiates a redraw using the most recently
    /// rendered scene
    pub fn request_redraw(&self, incremental: bool) -> Result<()> {
        self.0.send_event(incremental).map_err(Error::SendEvent)
    }
}

/// A 3d accelerated window
pub struct Window<EH: EventHandler> {
    /// User-supplied event handler
    handler: Option<EH>,

    /// Event loop for the window
    event_loop: Option<EventLoop<UserEvent>>,

    /// Window that we created
    window: winit::window::Window,

    /// Logical width of the inner part of the window
    width: u32,

    /// Logical height of the inner part of the window
    height: u32,

    /// Instant of last update
    last_update_inst: Instant,

    /// Device we're using to render with
    device: Device,

    /// Surface to draw on when rendering
    surface: Surface,

    /// Configuration for the surface
    config: SurfaceConfiguration,

    /// Queue to use for sending commands to the device
    queue: Queue,

    /// Internal camera mode
    camera_state: CameraState,

    /// Bind group for the camera
    camera_bind_group: BindGroup,

    /// Camera uniform buffer for the shader
    camera_buffer: Buffer,

    /// Tracks if the next redraw is incremental
    incremental: bool,

    /// Render pipeline for triangles
    triangle_pipeline: RenderPipeline,

    /// Render pipeline for line strips
    line_pipeline: RenderPipeline,

    /// Scene texture (saved from output on a non-incremental render)
    scene_texture: (Texture, TextureView),

    /// Scene depth (saved from output on a non-incremental render)
    scene_depth: (Texture, TextureView),

    /// Output texture (only for multi-sampling)
    output_texture: Option<(Texture, TextureView)>,

    /// Output depth
    output_depth: (Texture, TextureView),

    /// A proxy for the event loop so we can send commands from a thread
    proxy: EventLoopProxy<UserEvent>,

    /// Queued draw commands for drawing triangles to the scene
    persist_tri_commands: Vec<(Rc<Buffer>, Range<u32>)>,

    /// Queued draw commands for temporary triangles
    tri_commands: Vec<(Rc<Buffer>, Range<u32>)>,

    /// Queued draw commands for drawing lines to the scene
    persist_line_commands: Vec<(Rc<Buffer>, Range<u32>)>,

    /// Queued draw commands for temporary lines
    line_commands: Vec<(Rc<Buffer>, Range<u32>)>,
}

/// Create an infinite reversed-z perspective projection
pub fn infinite_reversed_perspective(fovy: impl Into<Rad<f32>>, aspect: f32, znear: f32) -> Matrix4<f32> {
    let h = 1.0 / (fovy.into() * 0.5).tan();
    let w = h / aspect;

    Matrix4::new(
        w,   0.0, 0.0,   0.0,
        0.0,  h,  0.0,   0.0,
        0.0, 0.0, 0.0,  -1.0,
        0.0, 0.0, znear, 0.0
    )
}

impl<EH: 'static + EventHandler> Window<EH> {
    /// Create a new window with a given `title`, `width` and `height` using
    /// MSAA level `msaa_level`
    ///
    /// The window will not be displayed until `window.run()` is invoked
    pub fn new(title: impl AsRef<str>,
            width: u32, height: u32, msaa_level: Msaa, vsync: Vsync)
                -> Result<Self> {
        // Create an event loop for window events
        let event_loop = EventLoop::with_user_event();

        // Create a window
        let window = WindowBuilder::new()
            .with_inner_size(LogicalSize::new(width, height))
            .with_title(title.as_ref())
            .with_resizable(false)
            .with_visible(false)
            .build(&event_loop).map_err(Error::CreateWindow)?;

        // Get the inner physical size of the window, as we originally used a
        // logical size to set the window size
        println!("Requested logical resolution: {:5}x{:5}", width, height);
        let width  = window.inner_size().width;
        let height = window.inner_size().height;
        println!("Got physical resolution:      {:5}x{:5}", width, height);

        // Create new instance of WGPU using a first-tier supported backend
        // Eg: Vulkan + Metal + DX12 + Browser WebGPU
        let instance = Instance::new(Backends::PRIMARY);

        // Create a surface for our Window. Unsafe since this uses raw window
        // handles and the window must remain valid for the lifetime of the
        // created surface
        //
        // A Surface represents a platform-specific surface (e.g. a window)
        // onto which rendered images may be presented.
        let surface = unsafe { instance.create_surface(&window) };

        // Get a handle to a physical graphics and/or compute device
        let adapter = pollster::block_on(async {
            instance.request_adapter(&RequestAdapterOptions {
                // Request the high performance graphics adapter, eg. pick the
                // discrete GPU over the integrated GPU
                power_preference: PowerPreference::HighPerformance,

                // Don't force fallback, we don't want software rendering :D
                force_fallback_adapter: false,

                // Make sure the adapter we request can render on `surface`
                compatible_surface: Some(&surface),
            }).await
        }).ok_or(Error::GetAdapter)?;

        // Display renderer information
        let adapter_info = adapter.get_info();
        println!("Renderer: {:04x}:{:04x} | {} | {:?} | {:?}",
            adapter_info.vendor, adapter_info.device,
            adapter_info.name,
            adapter_info.device_type, adapter_info.backend);

        // Create the logical device and command queue
        let (mut device, queue) = pollster::block_on(async {
            adapter.request_device(&DeviceDescriptor {
                // Debug label for the device
                label: None,

                // Features that the device should support
                features: Features::empty(),

                // Limits that the device should support. If any limit is
                // "better" than the limit exposed by the adapter, creating a
                // device will panic.
                limits: Limits::default(),
            }, None).await
        }).map_err(Error::CreateDevice)?;

        // Get the preferred texture format for the swapchain with the surface
        // and adapter we are using
        let swapchain_format = surface.get_preferred_format(&adapter)
            .ok_or(Error::PreferredFormat)?;

        // Load and compile the shaders
        let shader = device.create_shader_module(&ShaderModuleDescriptor {
            // Debugging label
            label:  None,

            // Shader source code
            source: ShaderSource::Wgsl(
                Cow::Borrowed(include_str!("shader.wgsl"))),
        });

        let config = SurfaceConfiguration {
            // Usage for the swap chain. In this case, this is currently the
            // only supported option.
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_DST,

            // Set the preferred texture format for the swap chain to be what
            // the surface and adapter want.
            format: swapchain_format,

            // Set the width of the swap chain
            width,

            // Set the height of the swap chain
            height,

            // The way data is presented to the screen
            // `Immediate` (no vsync)
            // `Mailbox`   (no vsync for rendering, but frames synced on vsync)
            // `Fifo`      (full vsync)
            present_mode: match vsync {
                Vsync::Off => PresentMode::Immediate,
                Vsync::On  => PresentMode::Fifo,
            },
        };

        // Configure the swap buffers
        surface.configure(&device, &config);

        // Create the camera buffer for supplying the camera uniform to the
        // shader
        let camera_buffer = device.create_buffer(&BufferDescriptor {
            // Debug label
            label: None,

            // Buffer size `mat4x4<f32>`
            size: 4 * 4 * 4,

            // Usage for the buffer
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,

            // Buffer not mapped at creation
            mapped_at_creation: false,
        });

        // Create the bind group layout for the camera projection matrix
        let camera_bind_group_layout = device.create_bind_group_layout(
            &BindGroupLayoutDescriptor {
                // Debug label
                label: None,

                entries: &[
                    BindGroupLayoutEntry {
                        // Bind as zero
                        binding: 0,

                        // We only need this for the vertex shader
                        visibility: ShaderStages::VERTEX,

                        // Set the typing as a uniform
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size:   None,
                        },

                        // Not an array
                        count: None,
                    }
                ],
            });

        // Create the bind group for the camera projection matrix
        let camera_bind_group = device.create_bind_group(&BindGroupDescriptor {
            // Debug label
            label: None,

            // Layout to use for camera bind group
            layout: &camera_bind_group_layout,
            entries: &[
                BindGroupEntry {
                    // Program binding zero
                    binding: 0,

                    // Set the binding to the camera buffer
                    resource: camera_buffer.as_entire_binding(),
                }
            ],
        });

        let msaa_level = msaa_level as u32;

        // Create scene texture and depth
        let scene_texture = Self::create_texture(
            &mut device, width, height, swapchain_format,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            msaa_level);

        let scene_depth =
            Self::create_texture(
            &mut device, width, height, TextureFormat::Depth32Float,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            msaa_level);

        // Create output texture (only when MSAA is enabled) and depth
        let output_texture = (msaa_level != 1).then(|| Self::create_texture(
            &mut device, width, height, swapchain_format,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_DST,
            msaa_level));

        let output_depth =
            Self::create_texture(
            &mut device, width, height, TextureFormat::Depth32Float,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_DST,
            msaa_level);

        // Create line pipeline
        let line_pipeline = Self::create_render_pipeline(
            &mut device,
            &camera_bind_group_layout,
            PrimitiveTopology::LineList,
            &shader,
            swapchain_format,
            msaa_level);

        // Create triangle pipeline
        let triangle_pipeline = Self::create_render_pipeline(
            &mut device,
            &camera_bind_group_layout,
            PrimitiveTopology::TriangleList,
            &shader,
            swapchain_format,
            msaa_level);

        // Create the window
        let mut ret = Self {
            handler: None,
            window,
            width,
            height,
            last_update_inst: Instant::now(),
            proxy: event_loop.create_proxy(),
            event_loop: Some(event_loop),
            device,
            surface,
            config,
            queue,
            incremental: false,
            camera_state: CameraState::None,
            camera_bind_group,
            camera_buffer,
            output_texture,
            output_depth,
            scene_texture,
            scene_depth,
            line_pipeline,
            triangle_pipeline,
            persist_tri_commands: Vec::new(),
            tri_commands: Vec::new(),
            persist_line_commands: Vec::new(),
            line_commands: Vec::new(),
        };

        // Set an initial camera state
        ret.update_camera(Point3::new(0., 0., 0.), Deg(0.), Deg(0.));

        Ok(ret)
    }

    /// Set the internal camera mode
    pub fn camera_mode(mut self, camera_mode: CameraMode) -> Self {
        // Set the camera state
        self.camera_state = match camera_mode {
            CameraMode::None => CameraState::None,
            CameraMode::Flight3d => {
                CameraState::Flight3d {
                    eye:     point3(0., 0., 0.),
                    pitch:   Deg(0.),
                    yaw:     Deg(0.),
                    speed:   250.,
                    panning: false,
                    key_w:   false,
                    key_a:   false,
                    key_s:   false,
                    key_d:   false,
                }
            }
        };

        self
    }

    /// Request a redraw trigger. This can be moved to other threads to trigger
    /// a redraw remotely
    pub fn redraw_trigger(&mut self) -> RedrawTrigger {
        RedrawTrigger(self.proxy.clone())
    }

    /// Push a draw command to the draw command list
    #[inline]
    pub fn push_command(&mut self, command: DrawCommand) {
        match command {
            DrawCommand::Triangles(Persist::Yes, buf, range) => {
                self.persist_tri_commands.push((buf, range));
            }
            DrawCommand::Triangles(Persist::No, buf, range) => {
                self.tri_commands.push((buf, range));
            }
            DrawCommand::Lines(Persist::Yes, buf, range) => {
                self.persist_line_commands.push((buf, range));
            }
            DrawCommand::Lines(Persist::No, buf, range) => {
                self.line_commands.push((buf, range));
            }
        }
    }

    /// Update the camera position, pitch (degrees), and yaw (degrees)
    pub fn update_camera(&mut self, eye: Point3<f32>,
            pitch: Deg<f32>, yaw: Deg<f32>) {
        // Update the 3d camera position if we're using one. This allows a
        // user to forcably set the position of the camera
        match &mut self.camera_state {
            CameraState::Flight3d { eye: x, pitch: y, yaw: z, .. } => {
                *x = eye;
                *y = pitch;
                *z = yaw;
            }
            _ => {}
        }

        // Compute the vector which is the direction the camera is facing
        let direction = Vector3::new(
            pitch.cos() * yaw.sin(), pitch.sin(), pitch.cos() * yaw.cos());

        // Create a look_to view matrix based on the pitch and yaw
        let view = Matrix4::look_to_rh(eye, direction, Vector3::unit_y());

        // Create a perspective with 45 degree FoV and a znear of 1
        let proj = infinite_reversed_perspective(Deg(45.),
            self.width as f32 / self.height as f32, 1.);

        // Create the uniform from the projection and view
        let camera_uniform = proj * view;

        // Update the camera uniform buffer
        let camera_uniform = unsafe {
            std::slice::from_raw_parts(
                std::ptr::addr_of!(camera_uniform) as *const u8,
                std::mem::size_of_val(&camera_uniform))
        };
        self.queue.write_buffer(&self.camera_buffer, 0, camera_uniform);

        // Request a full redraw
        self.request_redraw(false);
    }

    /// Request a redraw
    ///
    /// If `incremental` is set, initiates a redraw using the most recently
    /// rendered scene
    pub fn request_redraw(&mut self, incremental: bool) {
        // If anyone requests non-incremental, make sure we stay
        // non-incremental
        self.incremental &= incremental;

        if USE_REDRAW_REQUEST {
            // Request the redraw!
            self.window.request_redraw();
        }
    }

    /// Create a new vertex buffer with given `data`
    pub fn create_vertex_buffer(&mut self,
            data: impl AsRef<[Vertex]>) -> Rc<Buffer> {
        // Make a new buffer with the desired shape and contents
        Rc::new(self.device.create_buffer_init(&BufferInitDescriptor {
            // Debug label
            label: None,

            // Contents for the buffer
            contents: unsafe {
                std::slice::from_raw_parts(
                    data.as_ref().as_ptr() as *const u8,
                    std::mem::size_of_val(data.as_ref()))
            },

            // Usage of the buffer is vertex data
            usage: BufferUsages::VERTEX,
        }))
    }

    /// Create arendering pipeline for a given topology
    fn create_render_pipeline(
            device:                   &mut Device,
            camera_bind_group_layout: &BindGroupLayout,
            topology:                 PrimitiveTopology,
            shader:                   &ShaderModule,
            swapchain_format:         TextureFormat,
            msaa_level:               u32)
                -> RenderPipeline {
        // Create a render pipeline, mainly we have to do this to set the bind
        // groups
        let render_pipeline_layout = device.create_pipeline_layout(
            &PipelineLayoutDescriptor {
                // Label for debugging
                label: None,

                // Bind groups
                bind_group_layouts: &[
                    camera_bind_group_layout,
                ],

                // Constant ranges
                push_constant_ranges: &[],
            }
        );

        // Create a pipeline which applies the passes needed for rendering
        device.create_render_pipeline(&RenderPipelineDescriptor {
            // Debug label of the pipeline. This will show up in graphics
            // debuggers for easy identification.
            label: None,

            // The layout of bind groups for this pipeline.
            layout: Some(&render_pipeline_layout),

            // The compiled vertex stage, its entry point, and the input
            // buffers layout.
            vertex: VertexState {
                // Compiled shader
                module: shader,

                // Name of the function for the entry point
                entry_point: "vs_main",

                // Buffers to pass in
                buffers: &[VertexBufferLayout {
                    // Stride of elements in the buffer (in bytes)
                    array_stride: std::mem::size_of::<Vertex>() as u64,

                    // Step mode
                    step_mode: VertexStepMode::Vertex,

                    // Attributes to define the layout of the buffer
                    attributes: &[
                        // A `vec3<f32>` which contains X, Y, Z floats
                        VertexAttribute {
                            // Offset in the structure
                            offset: 0,

                            // Location to bind to in the shader
                            shader_location: 0,

                            // Format
                            format: VertexFormat::Float32x3,
                        },

                        // The R, G, B values as a `uvec4<u8>`
                        VertexAttribute {
                            // Offset in the structure
                            offset: 12,

                            // Location to bind to in the shader
                            shader_location: 1,

                            // Format
                            // Four unsigned bytes (u8). [0, 255] converted to
                            // float [0, 1] vec4 in shaders.
                            format: VertexFormat::Unorm8x4,
                        },
                    ],
                }],
            },

            // The properties of the pipeline at the primitive assembly and
            // rasterization level.
            primitive: PrimitiveState {
                topology,
                cull_mode: Some(Face::Back),
                ..Default::default()
            },

            // The compiled fragment stage, its entry point, and the color
            // targets.
            fragment: Some(FragmentState {
                // Compiled shader
                module: shader,

                // Name of the function for the entry point
                entry_point: "fs_main",

                // Type of output for the fragment shader (the correct texture
                // format that our GPU wants)
                targets: &[swapchain_format.into()],
            }),

            // The effect of draw calls on the depth and stencil aspects of the
            // output target, if any.
            depth_stencil: Some(DepthStencilState {
                // 32-bit floats for depth
                format: TextureFormat::Depth32Float,

                // Enable depth updates
                depth_write_enabled: true,

                // Depth comparison function
                depth_compare: CompareFunction::Greater,

                // Stencil
                stencil: StencilState::default(),

                // Bias
                bias: DepthBiasState::default(),
            }),

            // The multi-sampling properties of the pipeline.
            multisample: MultisampleState {
                count: msaa_level,
                mask:  !0,
                alpha_to_coverage_enabled: false,
            },

            // If the pipeline will be used with a multiview render pass, this
            // indicates how many array layers the attachments will have.
            multiview: None,
        })
    }

    /// Create a texture with a given width, height, format, usages, and MSAA
    fn create_texture(device: &mut Device,
            width: u32, height: u32, format: TextureFormat,
            usage: TextureUsages, msaa_level: u32) -> (Texture, TextureView) {
        // Create the texture
        let texture = device.create_texture(&TextureDescriptor {
            label:           None,
            mip_level_count: 1,
            sample_count:    msaa_level,
            dimension:       TextureDimension::D2,

            size: Extent3d {
                width:  width,
                height: height,
                depth_or_array_layers: 1
            },

            format,
            usage,
        });

        // Create the view
        let view = texture.create_view(&TextureViewDescriptor::default());

        // Return the result
        (texture, view)
    }

    fn render(&mut self, handler: &mut EH) -> Result<()> {
        // Notify about the frame
        handler.render(self, self.incremental);

        // Get the next available surface in the swap chain
        let frame = self.surface.get_current_texture().or_else(|_| {
            self.surface.configure(&self.device, &self.config);
            self.surface.get_current_texture()
        }).map_err(Error::GetSurfaceTexture)?;

        let frame_texture = (&frame.texture, &frame.texture
            .create_view(&TextureViewDescriptor::default()));

        // Create an encoder for a series of GPU operations
        let mut encoder = self.device.create_command_encoder(
            &CommandEncoderDescriptor::default());

        let (render_texture, resolve_target) = if let Some((output_texture, output_view)) = &self.output_texture {
            ((output_texture, output_view), Some(frame_texture.1))
        } else {
            (frame_texture, None)
        };

        // If we're doing an incremental render, copy the last rendered
        // scene as the base for the new rendering
        if !self.incremental {
            // Start a render pass
            let mut render_pass = encoder.begin_render_pass(
                &RenderPassDescriptor {
                    // Debug label
                    label: None,

                    // Description of the output color buffer
                    color_attachments: &[RenderPassColorAttachment {
                        // Draw either to the MSAA buffer or the view,
                        // depending on if MSAA is enabled
                        view: &self.scene_texture.1,

                        // Actual screen to render to
                        resolve_target: None,

                        // Clear the screen to black at the start of
                        // the rendering pass
                        ops: Operations {
                            load: LoadOp::Clear(Color::BLACK),
                            store: true,
                        },
                    }],

                    // Description of the depth buffer
                    depth_stencil_attachment: Some(
                            RenderPassDepthStencilAttachment {
                        // Depth buffer
                        view: &self.scene_depth.1,

                        // Reset the depth buffer to `0.0` for all
                        // values at the start of the rendering pass
                        depth_ops: Some(Operations {
                            // Clear to 0.0
                            load: LoadOp::Clear(0.0),
                            store: true,
                        }),

                        // Operations to perform on the stencil
                        stencil_ops: None,
                    }),
                });

            // Bind the camera
            render_pass.set_bind_group(
                0, &self.camera_bind_group, &[]);

            // Use the triangle pipeline
            render_pass.set_pipeline(&self.triangle_pipeline);

            // Render persistant data
            for (buffer, range) in &self.persist_tri_commands {
                // Bind the vertex buffer
                render_pass
                    .set_vertex_buffer(0, buffer.slice(..));

                // Draw it!
                render_pass.draw(range.clone(), 0..1);
            }

            // Use the line pipeline
            render_pass.set_pipeline(&self.line_pipeline);

            // Render persistant data
            for (buffer, range) in &self.persist_line_commands {
                // Bind the vertex buffer
                render_pass
                    .set_vertex_buffer(0, buffer.slice(..));

                // Draw it!
                render_pass.draw(range.clone(), 0..1);
            }
        }

        encoder.copy_texture_to_texture(
            self.scene_texture.0.as_image_copy(),
            render_texture.0.as_image_copy(),
            Extent3d {
                width:  self.width,
                height: self.height,
                depth_or_array_layers: 1
            });

        encoder.copy_texture_to_texture(
            self.scene_depth.0.as_image_copy(),
            self.output_depth.0.as_image_copy(),
            Extent3d {
                width:  self.width,
                height: self.height,
                depth_or_array_layers: 1
            });

        {
            // Start a render pass
            let mut render_pass = encoder.begin_render_pass(
                &RenderPassDescriptor {
                    // Debug label
                    label: None,

                    // Description of the output color buffer
                    color_attachments: &[RenderPassColorAttachment {
                        // Draw either to the MSAA buffer or the view,
                        // depending on if MSAA is enabled
                        view: &render_texture.1,

                        // Actual screen to render to
                        resolve_target: resolve_target,

                        // Don't clear color data
                        ops: Operations {
                            load:  LoadOp::Load,
                            store: true,
                        },
                    }],

                    // Description of the depth buffer
                    depth_stencil_attachment: Some(
                            RenderPassDepthStencilAttachment {
                        // Depth buffer
                        view: &self.output_depth.1,

                        // Reset the depth buffer to `1.0` for all
                        // values at the start of the rendering pass
                        depth_ops: Some(Operations {
                            // Don't clear depth buffer
                            load:  LoadOp::Load,
                            store: true,
                        }),

                        // Operations to perform on the stencil
                        stencil_ops: None,
                    }),
                });

            // Bind the camera
            render_pass.set_bind_group(
                0, &self.camera_bind_group, &[]);

            // Use the triangle pipeline
            render_pass.set_pipeline(&self.triangle_pipeline);

            // Render persistant data
            for (buffer, range) in &self.tri_commands {
                // Bind the vertex buffer
                render_pass
                    .set_vertex_buffer(0, buffer.slice(..));

                // Draw it!
                render_pass.draw(range.clone(), 0..1);
            }

            // Use the line pipeline
            render_pass.set_pipeline(&self.line_pipeline);

            // Render persistant data
            for (buffer, range) in &self.line_commands {
                // Bind the vertex buffer
                render_pass
                    .set_vertex_buffer(0, buffer.slice(..));

                // Draw it!
                render_pass.draw(range.clone(), 0..1);
            }
        }

        // Done with commands, discard them
        self.persist_tri_commands.clear();
        self.tri_commands.clear();
        self.persist_line_commands.clear();
        self.line_commands.clear();

        // Finalize the encoder and submit the buffer for execution
        self.queue.submit(Some(encoder.finish()));

        // Present the frame to the output surface
        frame.present();

        // Now that the frame was drawn, we can go back to incremental
        // mode
        self.incremental = true;

        Ok(())
    }

    /// Handle the events from the event loop
    fn handle_event(&mut self, event: Event<UserEvent>,
            control_flow: &mut ControlFlow) -> Result<()> {
        *control_flow = ControlFlow::Poll;

        // Get the handler
        let mut handler = self.handler.take().unwrap();

        // Handle events
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                // Exit when the user closes the window
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent { event: WindowEvent::KeyboardInput {
                input, ..
            }, ..} => {
                match (input.state, input.virtual_keycode,
                        &mut self.camera_state) {
                    (ElementState::Pressed, Some(VirtualKeyCode::W),
                            CameraState::Flight3d { key_w, .. }) => {
                        *key_w = true;
                    }
                    (ElementState::Released, Some(VirtualKeyCode::W),
                            CameraState::Flight3d { key_w, .. }) => {
                        *key_w = false;
                    }
                    (ElementState::Pressed, Some(VirtualKeyCode::A),
                            CameraState::Flight3d { key_a, .. }) => {
                        *key_a = true;
                    }
                    (ElementState::Released, Some(VirtualKeyCode::A),
                            CameraState::Flight3d { key_a, .. }) => {
                        *key_a = false;
                    }
                    (ElementState::Pressed, Some(VirtualKeyCode::S),
                            CameraState::Flight3d { key_s, .. }) => {
                        *key_s = true;
                    }
                    (ElementState::Released, Some(VirtualKeyCode::S),
                            CameraState::Flight3d { key_s, .. }) => {
                        *key_s = false;
                    }
                    (ElementState::Pressed, Some(VirtualKeyCode::D),
                            CameraState::Flight3d { key_d, .. }) => {
                        *key_d = true;
                    }
                    (ElementState::Released, Some(VirtualKeyCode::D),
                            CameraState::Flight3d { key_d, .. }) => {
                        *key_d = false;
                    }
                    (ElementState::Pressed, Some(key), _) => {
                        // Pass through key
                        handler.key_down(key);
                    }
                    (ElementState::Released, Some(key), _) => {
                        // Pass through key
                        handler.key_up(key);
                    }
                    _ => {}
                }
            }
            Event::DeviceEvent { event: MouseMotion { delta: (x, y) }, .. } =>{
                match &mut self.camera_state {
                    CameraState::Flight3d {
                        eye, pitch, yaw, panning: true, ..
                    } => {
                        // Update the pitch (clamping to [-89, 89])
                        *pitch = Deg((pitch.0 + (-y as f32 / 5.))
                            .clamp(-89., 89.));

                        // Update yaw
                        *yaw += Deg(-x as f32 / 5.);

                        // Update the camera
                        let eye   = *eye;
                        let pitch = *pitch;
                        let yaw   = *yaw;
                        self.update_camera(eye, pitch, yaw);
                    }
                    _ => {
                        // Pass through movement
                        handler.mouse_move(self, x, y);
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::MouseInput {
                state: ElementState::Pressed, button, ..
            }, ..} => {
                match (button, &mut self.camera_state) {
                    (MouseButton::Left, CameraState::Flight3d { panning, .. })
                            => {
                        // We're panning
                        self.window.set_cursor_grab(true)
                            .map_err(Error::CursorGrab)?;
                        self.window.set_cursor_visible(false);
                        *panning = true;
                    }
                    _ => {
                        // Pass through mouse event
                        handler.mouse_down(self, button);
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::MouseInput {
                state: ElementState::Released, button, ..
            }, ..} => {
                match (button, &mut self.camera_state) {
                    (MouseButton::Left, CameraState::Flight3d { panning, .. })
                            => {
                        // We're no longer panning
                        self.window.set_cursor_grab(false)
                            .map_err(Error::CursorGrab)?;
                        self.window.set_cursor_visible(true);
                        *panning = false;
                    }
                    _ => {
                        // Pass through mouse event
                        handler.mouse_up(self, button);
                    }
                }
            }
            Event::WindowEvent { event: WindowEvent::MouseWheel {
                delta: MouseScrollDelta::LineDelta(_, y), .. }, ..
            } => {
                if let CameraState::Flight3d { ref mut speed, .. } =
                        self.camera_state {
                    // Update speed for camera movement
                    if y > 0. {
                        *speed *= 2.;
                    } else {
                        *speed /= 2.;
                    }
                } else {
                    handler.mouse_scroll(self, y);
                }
            }
            Event::RedrawRequested(_) => {
                self.render(&mut handler)?;
            }
            Event::MainEventsCleared => {
                let update_delta = self.last_update_inst.elapsed().as_secs_f32();
                self.last_update_inst = Instant::now();

                if let CameraState::Flight3d {
                    key_w, key_a, key_s, key_d,
                    ref mut eye, speed, pitch, yaw, ..
                } = self.camera_state {
                    let speed = speed * update_delta;

                    // Check for movement keypresses
                    if key_w || key_a || key_s || key_d {
                        // Key is pressed, so we have to redraw so we can
                        // handle movement

                        // Determine movement for forwards and strafing
                        let forwards = match (key_w, key_s) {
                            (true, true)   =>  0.,
                            (true, false)  =>  speed,
                            (false, true)  => -speed,
                            (false, false) =>  0.,
                        };
                        let strafe = match (key_a, key_d) {
                            (true, true)   =>  0.,
                            (true, false)  =>  speed,
                            (false, true)  => -speed,
                            (false, false) =>  0.,
                        };

                        // Update the camera eye position...

                        // Compute the vector which is the direction the camera
                        // is facing
                        let direction = Vector3::new(
                            pitch.cos() * yaw.sin(), pitch.sin(),
                            pitch.cos() * yaw.cos());

                        // Compute the vector which would give strafing to the
                        // left of the camera
                        let strafe_pitch = Deg(0.);
                        let strafe_yaw   = yaw + Deg(90.);
                        let strafe_direction = Vector3::new(
                            strafe_pitch.cos() * strafe_yaw.sin(),
                            strafe_pitch.sin(),
                            strafe_pitch.cos() * strafe_yaw.cos());

                        // Update camera position
                        *eye += direction * forwards;
                        *eye += strafe_direction * strafe;
                        let eye = *eye;
                        self.update_camera(eye, pitch, yaw);
                    }
                }

                // Check if we should schedule another frame for drawing
                handler.should_redraw(self);

                if !USE_REDRAW_REQUEST {
                    // Always render, relying on vsync instead of RedrawRequested to reduce CPU usage
                    // Vsync+RedrawRequested limits FPS to ~60 on Windows
                    self.render(&mut handler)?;
                }
            }
            Event::UserEvent(incremental) => {
                // Got a remote request to redraw the screen
                self.request_redraw(incremental);
            }
            _ => {
                // Unhandled event
            }
        }

        // Put the handler back
        self.handler = Some(handler);

        Ok(())
    }

    /// Set the window visible and start the event loop
    pub fn run<'a>(mut self) -> ! {
        // Register the event handler
        self.handler = Some(<EH>::create(&mut self));

        // Make the window visible
        self.window.set_visible(true);

        self.event_loop.take().unwrap().run(move |event, _, control_flow| {
            // Handle events forever, unless we get an error
            self.handle_event(event, control_flow)
                .expect("Failed to handle event");
        });
    }
}

