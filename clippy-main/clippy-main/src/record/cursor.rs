//! El cursor del mouse en los clips del escritorio. Desktop Duplication no lo pone en la imagen: avisa en cada
//! cuadro dónde está y, cuando cambia, su forma. Se dibuja sobre la captura en la GPU antes de pasarla a NV12, con un
//! shader mínimo en dos pasadas: lo que tapa (con transparencia) y lo que invierte la imagen de abajo (el cursor de
//! texto, los monocromos). Antes de dibujarlo se guarda lo que había debajo y se repone al moverlo, así no deja rastro
//! sobre una imagen quieta.

use std::ffi::c_void;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use windows::core::{s, w, Interface, HRESULT, PCSTR};
use windows::Win32::Graphics::Direct3D::{ID3DBlob, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
    DXGI_MODE_ROTATION_ROTATE90, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use super::capture::Frame;

/// Un rectángulo del tamaño de la textura que se lee, ubicado con el viewport donde va el cursor.
const SHADER: &str = r"
Texture2D img;
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD; };
V vs(uint id : SV_VertexID) {
    V v;
    v.uv = float2(id & 1, id >> 1);
    v.pos = float4(v.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return v;
}
float4 ps(V v) : SV_Target {
    uint w, h;
    img.GetDimensions(w, h);
    return img.Load(int3(v.uv * float2(w, h), 0));
}
";

type Compile = unsafe extern "system" fn(
    *const c_void,
    usize,
    PCSTR,
    *const c_void,
    *mut c_void,
    PCSTR,
    PCSTR,
    u32,
    u32,
    *mut *mut c_void,
    *mut *mut c_void,
) -> HRESULT;

/// `SHADER` compilado.
struct Code {
    vs: Vec<u8>,
    ps: Vec<u8>,
}

/// Se compila una vez por proceso. d3dcompiler_47 viene con Windows: se carga recién acá para que con el cursor
/// apagado ni se cargue.
fn shaders() -> Result<&'static Code> {
    static CODE: OnceLock<Result<Code, String>> = OnceLock::new();
    CODE.get_or_init(|| compile().map_err(|e| e.to_string())).as_ref().map_err(|e| anyhow!("{e}"))
}

fn compile() -> Result<Code> {
    unsafe {
        let lib = LoadLibraryW(w!("d3dcompiler_47.dll"))?;
        let f = GetProcAddress(lib, s!("D3DCompile")).ok_or_else(|| anyhow!("d3dcompiler_47.dll sin D3DCompile"))?;
        let f: Compile = std::mem::transmute(f);
        let bytes = |b: &ID3DBlob| std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec();
        let one = |entry: PCSTR, target: PCSTR| -> Result<Vec<u8>> {
            let (mut code, mut errors) = (std::ptr::null_mut(), std::ptr::null_mut());
            let hr = f(
                SHADER.as_ptr().cast(),
                SHADER.len(),
                PCSTR::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                entry,
                target,
                0,
                0,
                &mut code,
                &mut errors,
            );
            let blob = |p: *mut c_void| (!p.is_null()).then(|| ID3DBlob::from_raw(p));
            match (blob(code), blob(errors)) {
                (Some(code), _) if hr.is_ok() => Ok(bytes(&code)),
                (_, errors) => Err(anyhow!("{hr} {}", errors.map(|e| String::from_utf8_lossy(&bytes(&e)).into_owned()).unwrap_or_default())),
            }
        };
        Ok(Code { vs: one(s!("vs"), s!("vs_4_0"))?, ps: one(s!("ps"), s!("ps_4_0"))? })
    }
}

/// Forma del cursor como la manda Desktop Duplication.
struct Shape {
    info: DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    data: Vec<u8>,
}

impl Shape {
    fn mono(&self) -> bool {
        self.info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32
    }

    /// Tamaño como se ve: el monocromo trae sus dos máscaras una arriba de la otra.
    fn size(&self) -> (u32, u32) {
        (self.info.Width, if self.mono() { self.info.Height / 2 } else { self.info.Height })
    }

    /// Que los datos alcancen para el tamaño que dice (si no, no se dibuja).
    fn valid(&self) -> bool {
        let (w, h) = self.size();
        let row = if self.mono() { w.div_ceil(8) } else { w * 4 };
        w > 0 && h > 0 && self.info.Pitch >= row && self.data.len() as u64 >= self.info.Pitch as u64 * self.info.Height as u64
    }

    /// Píxeles BGRA de las dos pasadas, como se ven: lo que tapa (con transparencia) y lo que invierte la imagen de
    /// abajo (None si no hay nada que invertir).
    fn layers(&self) -> (Vec<[u8; 4]>, Option<Vec<[u8; 4]>>) {
        let (w, h) = self.size();
        let (w, h, pitch) = (w as usize, h as usize, self.info.Pitch as usize);
        let masked = self.info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 as u32;
        let (mut cover, mut invert) = (Vec::with_capacity(w * h), Vec::with_capacity(w * h));
        for y in 0..h {
            for x in 0..w {
                let (c, i) = if self.mono() {
                    // Máscara AND arriba y XOR abajo, un bit por píxel.
                    let bit = |row: usize| self.data[row * pitch + x / 8] >> (7 - x % 8) & 1 == 1;
                    match (bit(y), bit(y + h)) {
                        (false, false) => ([0, 0, 0, 255], [0; 4]),
                        (false, true) => ([255; 4], [0; 4]),
                        (true, false) => ([0; 4], [0; 4]),
                        (true, true) => ([0; 4], [255, 255, 255, 0]),
                    }
                } else {
                    let p = &self.data[y * pitch + x * 4..][..4];
                    match (masked, p[3]) {
                        // Color enmascarado: con máscara 0 reemplaza el píxel; con 0xFF lo combina (XOR) con el de abajo.
                        (true, 0) => ([p[0], p[1], p[2], 255], [0; 4]),
                        (true, _) => ([0; 4], [p[0], p[1], p[2], 0]),
                        (false, _) => ([p[0], p[1], p[2], p[3]], [0; 4]),
                    }
                };
                cover.push(c);
                invert.push(i);
            }
        }
        let any = invert.iter().any(|p| p[..3] != [0, 0, 0]);
        (cover, any.then_some(invert))
    }
}

fn turned(rotation: DXGI_MODE_ROTATION) -> bool {
    matches!(rotation, DXGI_MODE_ROTATION_ROTATE90 | DXGI_MODE_ROTATION_ROTATE270)
}

/// Un rectángulo del monitor como se ve (`screen` de tamaño) en la imagen capturada, que llega sin rotar: con
/// `ROTATE90` hay que girarla un cuarto en el sentido del reloj para verla.
fn unrotate(rotation: DXGI_MODE_ROTATION, (x, y, w, h): (i32, i32, i32, i32), (sw, sh): (i32, i32)) -> (i32, i32, i32, i32) {
    match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => (y, sw - x - w, h, w),
        DXGI_MODE_ROTATION_ROTATE180 => (sw - x - w, sh - y - h, w, h),
        DXGI_MODE_ROTATION_ROTATE270 => (sh - y - h, x, h, w),
        _ => (x, y, w, h),
    }
}

/// `px` (`w` x `h`, como se ve) girado como la imagen capturada (ver `unrotate`).
fn turn(px: &[[u8; 4]], (w, h): (u32, u32), rotation: DXGI_MODE_ROTATION) -> Vec<[u8; 4]> {
    let (w, h) = (w as usize, h as usize);
    let (tw, th) = if turned(rotation) { (h, w) } else { (w, h) };
    let mut out = Vec::with_capacity(w * h);
    for v in 0..th {
        for u in 0..tw {
            let (x, y) = match rotation {
                DXGI_MODE_ROTATION_ROTATE90 => (w - 1 - v, u),
                DXGI_MODE_ROTATION_ROTATE180 => (w - 1 - u, h - 1 - v),
                DXGI_MODE_ROTATION_ROTATE270 => (v, h - 1 - u),
                _ => (u, v),
            };
            out.push(px[y * w + x]);
        }
    }
    out
}

/// Una pasada del cursor: la textura que se lee y su vista.
struct Layer {
    tex: ID3D11Texture2D,
    view: ID3D11ShaderResourceView,
}

/// Texturas para dibujar formas de un tamaño (ya girado) sobre imágenes de un formato. Una forma nueva del mismo
/// tamaño (los cursores animados cambian varias veces por segundo) solo reescribe su contenido.
struct Ready {
    size: (u32, u32),
    format: DXGI_FORMAT,
    /// Número de forma y rotación de lo que tienen escrito.
    written: Option<(u32, DXGI_MODE_ROTATION)>,
    cover: Layer,
    invert: Layer,
    /// La forma invierte algo (si no, la segunda pasada no se hace).
    inverts: bool,
    /// Lo que queda debajo del cursor, para reponerlo.
    under: ID3D11Texture2D,
}

impl Ready {
    fn new(device: &ID3D11Device, size: (u32, u32), format: DXGI_FORMAT) -> Result<Self> {
        let layer = || -> Result<Layer> {
            let tex = texture(device, size, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_SHADER_RESOURCE)?;
            let mut view = None;
            unsafe { device.CreateShaderResourceView(&tex, None, Some(&mut view))? };
            Ok(Layer { view: view.ok_or_else(|| anyhow!("sin vista del cursor"))?, tex })
        };
        Ok(Self {
            size,
            format,
            written: None,
            cover: layer()?,
            invert: layer()?,
            inverts: false,
            under: texture(device, size, format, D3D11_BIND_FLAG(0))?,
        })
    }

    /// Escribe la forma número `n`, girada como la imagen.
    fn write(&mut self, context: &ID3D11DeviceContext, shape: &Shape, n: u32, rotation: DXGI_MODE_ROTATION) {
        let (cover, invert) = shape.layers();
        let put = |layer: &Layer, px: &[[u8; 4]]| unsafe {
            context.UpdateSubresource(&layer.tex, 0, None, turn(px, shape.size(), rotation).as_ptr().cast(), self.size.0 * 4, 0);
        };
        put(&self.cover, &cover);
        if let Some(px) = &invert {
            put(&self.invert, px);
        }
        self.inverts = invert.is_some();
        self.written = Some((n, rotation));
    }
}

fn texture(device: &ID3D11Device, (w, h): (u32, u32), format: DXGI_FORMAT, bind: D3D11_BIND_FLAG) -> Result<ID3D11Texture2D> {
    let d = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut t = None;
    unsafe { device.CreateTexture2D(&d, None, Some(&mut t))? };
    t.ok_or_else(|| anyhow!("sin textura del cursor"))
}

/// Dónde va el cursor en la imagen.
#[derive(Clone, Copy, PartialEq)]
struct Place {
    /// Rectángulo entero del cursor (puede salirse de la imagen).
    rect: (i32, i32, i32, i32),
    /// La parte adentro de la imagen.
    region: D3D11_BOX,
    shape: u32,
}

/// Dónde quedó dibujado el cursor y con qué se repone la imagen.
struct Drawn {
    tex: ID3D11Texture2D,
    place: Place,
    under: ID3D11Texture2D,
}

pub struct Cursor {
    device: ID3D11Device,
    /// El encoder usa el mismo dispositivo desde sus hilos: el dibujo (varias llamadas) va de una.
    lock: Option<ID3D11Multithread>,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cover: ID3D11BlendState,
    invert: ID3D11BlendState,
    /// Lo último que avisó Desktop Duplication: si se ve en este monitor y dónde está la esquina de arriba a la
    /// izquierda de la forma (no la punta), en el monitor como se ve.
    visible: bool,
    pos: (i32, i32),
    shape: Option<Shape>,
    /// Cuántas formas llegaron (para saber si la dibujada es la de ahora).
    shape_n: u32,
    ready: Option<Ready>,
    drawn: Option<Drawn>,
    target: Option<(ID3D11Texture2D, ID3D11RenderTargetView)>,
}

impl Cursor {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let code = shaders()?;
        unsafe {
            let (mut vs, mut ps) = (None, None);
            device.CreateVertexShader(&code.vs, None, Some(&mut vs))?;
            device.CreatePixelShader(&code.ps, None, Some(&mut ps))?;
            Ok(Self {
                device: device.clone(),
                lock: device.cast().ok(),
                vs: vs.ok_or_else(|| anyhow!("sin shader del cursor"))?,
                ps: ps.ok_or_else(|| anyhow!("sin shader del cursor"))?,
                // Tapar: transparencia común. Invertir: blanco da el negativo de lo de abajo y negro lo deja igual.
                cover: blend(device, D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_INV_SRC_ALPHA)?,
                invert: blend(device, D3D11_BLEND_INV_DEST_COLOR, D3D11_BLEND_INV_SRC_COLOR)?,
                visible: false,
                pos: (0, 0),
                shape: None,
                shape_n: 0,
                ready: None,
                drawn: None,
                target: None,
            })
        }
    }

    /// Lee dónde está el cursor y, si cambió, su forma. Va antes de soltar el cuadro (`ReleaseFrame`).
    pub fn update(&mut self, dup: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO) {
        if info.LastMouseUpdateTime != 0 {
            let p = info.PointerPosition;
            self.visible = p.Visible.as_bool();
            self.pos = (p.Position.x, p.Position.y);
        }
        if info.PointerShapeBufferSize == 0 {
            return;
        }
        let mut data = vec![0u8; info.PointerShapeBufferSize as usize];
        let (mut need, mut si) = (0, DXGI_OUTDUPL_POINTER_SHAPE_INFO::default());
        if unsafe { dup.GetFramePointerShape(data.len() as u32, data.as_mut_ptr().cast(), &mut need, &mut si) }.is_ok() {
            let shape = Shape { info: si, data };
            self.shape = shape.valid().then_some(shape);
            self.shape_n += 1;
        }
    }

    /// Se volvió a abrir la duplicación (otro monitor, cambio de modo): hasta que avise, no se sabe dónde está.
    pub fn reset(&mut self) {
        self.visible = false;
    }

    /// La imagen se reemplazó entera: el cursor dibujado ya no está.
    pub fn forget(&mut self) {
        self.drawn = None;
    }

    /// Deja `frame` con el cursor dibujado si `show` y Windows lo muestra en este monitor; si no, sin cursor.
    pub fn draw(&mut self, context: &ID3D11DeviceContext, frame: &Frame, show: bool) -> Result<()> {
        let place = if show && self.visible { self.place(context, frame)? } else { None };
        let same = match &self.drawn {
            Some(d) => d.tex == frame.tex && Some(d.place) == place,
            None => place.is_none(),
        };
        if same {
            return Ok(());
        }
        let target = match place {
            Some(_) => Some(self.target(&frame.tex)?),
            None => None,
        };
        unsafe {
            if let Some(l) = &self.lock {
                l.Enter();
            }
            if let Some(d) = self.drawn.take().filter(|d| d.tex == frame.tex) {
                let r = d.place.region;
                let all = D3D11_BOX { left: 0, top: 0, front: 0, right: r.right - r.left, bottom: r.bottom - r.top, back: 1 };
                context.CopySubresourceRegion(&frame.tex, 0, r.left, r.top, 0, &d.under, 0, Some(&all));
            }
            if let (Some(place), Some(target), Some(ready)) = (place, target, &self.ready) {
                context.CopySubresourceRegion(&ready.under, 0, 0, 0, 0, &frame.tex, 0, Some(&place.region));
                let (x, y, w, h) = place.rect;
                context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
                context.IASetInputLayout(None);
                context.VSSetShader(&self.vs, None);
                context.PSSetShader(&self.ps, None);
                context.RSSetState(None);
                let vp = D3D11_VIEWPORT { TopLeftX: x as f32, TopLeftY: y as f32, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 };
                context.RSSetViewports(Some(&[vp]));
                context.OMSetRenderTargets(Some(&[Some(target)]), None);
                let passes = [(&ready.cover, &self.cover), (&ready.invert, &self.invert)];
                for (layer, blend) in passes.into_iter().take(1 + ready.inverts as usize) {
                    context.PSSetShaderResources(0, Some(&[Some(layer.view.clone())]));
                    context.OMSetBlendState(blend, None, u32::MAX);
                    context.Draw(4, 0);
                }
                context.PSSetShaderResources(0, Some(&[None]));
                context.OMSetRenderTargets(None, None);
                self.drawn = Some(Drawn { tex: frame.tex.clone(), place, under: ready.under.clone() });
            }
            if let Some(l) = &self.lock {
                l.Leave();
            }
        }
        Ok(())
    }

    /// Dónde va el cursor en `frame` (None si no hay forma o queda afuera). Prepara la forma si cambió.
    fn place(&mut self, context: &ID3D11DeviceContext, frame: &Frame) -> Result<Option<Place>> {
        let Some(shape) = &self.shape else { return Ok(None) };
        let mut d = D3D11_TEXTURE2D_DESC::default();
        unsafe { frame.tex.GetDesc(&mut d) };
        let (sw, sh) = shape.size();
        let size = if turned(frame.rotation) { (sh, sw) } else { (sw, sh) };
        let ready = match &mut self.ready {
            Some(r) if (r.size, r.format) == (size, d.Format) => r,
            slot => slot.insert(Ready::new(&self.device, size, d.Format)?),
        };
        if ready.written != Some((self.shape_n, frame.rotation)) {
            ready.write(context, shape, self.shape_n, frame.rotation);
        }
        let (iw, ih) = (d.Width as i32, d.Height as i32);
        let screen = if turned(frame.rotation) { (ih, iw) } else { (iw, ih) };
        let rect = unrotate(frame.rotation, (self.pos.0, self.pos.1, sw as i32, sh as i32), screen);
        let (x0, y0) = (rect.0.max(0), rect.1.max(0));
        let (x1, y1) = ((rect.0 + rect.2).min(iw), (rect.1 + rect.3).min(ih));
        if x0 >= x1 || y0 >= y1 {
            return Ok(None);
        }
        let region = D3D11_BOX { left: x0 as u32, top: y0 as u32, front: 0, right: x1 as u32, bottom: y1 as u32, back: 1 };
        Ok(Some(Place { rect, region, shape: self.shape_n }))
    }

    /// Vista para dibujar sobre `tex` (la misma mientras no cambie la textura de la captura).
    fn target(&mut self, tex: &ID3D11Texture2D) -> Result<ID3D11RenderTargetView> {
        if let Some((t, v)) = &self.target {
            if t == tex {
                return Ok(v.clone());
            }
        }
        let mut v = None;
        unsafe { self.device.CreateRenderTargetView(tex, None, Some(&mut v))? };
        let v = v.ok_or_else(|| anyhow!("sin vista de la captura"))?;
        self.target = Some((tex.clone(), v.clone()));
        Ok(v)
    }
}

fn blend(device: &ID3D11Device, src: D3D11_BLEND, dest: D3D11_BLEND) -> Result<ID3D11BlendState> {
    let mut d = D3D11_BLEND_DESC::default();
    d.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
        BlendEnable: true.into(),
        SrcBlend: src,
        DestBlend: dest,
        BlendOp: D3D11_BLEND_OP_ADD,
        // El alfa de la imagen queda como estaba.
        SrcBlendAlpha: D3D11_BLEND_ZERO,
        DestBlendAlpha: D3D11_BLEND_ONE,
        BlendOpAlpha: D3D11_BLEND_OP_ADD,
        RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
    };
    let mut s = None;
    unsafe { device.CreateBlendState(&d, Some(&mut s))? };
    s.ok_or_else(|| anyhow!("sin mezcla del cursor"))
}
