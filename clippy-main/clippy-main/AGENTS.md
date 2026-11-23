# Clippy (app) — guía para agentes

Grabador de repeticiones instantáneas para Windows en Rust (eframe/egui 0.29 glow, windows 0.58).
Ver `README.md` para el mapa del código y los modos del exe.

## Reglas

- No sumes dependencias si Windows ya trae la API.
- **Que se comporte como una app de verdad** (Medal, NVIDIA Instant Replay): nada hardcodeado
  para un juego, nada que el usuario tenga que configurar si se puede detectar.
- **Textos de la interfaz en tres idiomas**: español rioplatense ("vos", "tocá", "apretá"), inglés y
  portugués de Brasil. Cada texto va donde se usa con `tr!(es, en, pt)` o, con valores, `trf!` (ver
  `src/i18n.rs`); nunca un literal suelto en la interfaz. Los errores que llegan a la pantalla se traducen
  donde se crean (`bail!(tr!(…))`), así que en `clippy.log` quedan en el idioma de la interfaz; el resto del
  registro y los comentarios, en español.
- No publicar releases sin que el usuario lo pida.

## Flujo de trabajo

Todo cambio entra a `main` por PR; nunca se commitea directo en `main`.

- **Primero el issue, después la PR.** Cada PR cierra su issue con `Closes #N`; si no existe, abrilo antes de
  crear la rama (ver "Issues y PRs"). Solo las `chore/` de versión o CI van sin issue.
- **Cada cosa nueva arranca de `main` al día y en un worktree propio**, nunca en el checkout principal:

  ```bash
  git switch main && git pull --ff-only
  git worktree add .claude/worktrees/<nombre> -b <tipo>/<nombre> main
  ```

  Si el checkout principal está en otra rama o tiene cambios sin commitear, no lo toques: `git fetch origin`
  y creá el worktree desde `origin/main`.
- **Ramas `<tipo>/<nombre>`**: `feature/` (algo nuevo), `fix/` (un bug), `chore/` (versiones, CI,
  dependencias, limpieza) o `docs/` (solo documentación). `<nombre>` corto, en español y en kebab-case, dice
  qué cambia (`feature/cursor-en-escritorio`, `fix/encoder-1366`); el worktree se llama igual.
- Una rama por cambio y PRs chicos. El primer `cargo build` en un worktree nuevo compila todo de cero.
- Cuando la PR se mergea, borrá el worktree: `git worktree remove .claude/worktrees/<nombre>`.
- **Commits y PRs sin atribución de IA**: nada de `Co-Authored-By`, "Generated with" ni links de sesión.

## Issues y PRs

Todas siguen las plantillas de `.github/`, ya sea que las abra una persona en GitHub o un agente con `gh`.
Con `gh`, armá el cuerpo con las mismas secciones que la plantilla.

- **Issues**: `.github/ISSUE_TEMPLATE/bug.md` (algo no anda) y `mejora.md` (algo nuevo o distinto). El título
  dice el síntoma o el resultado, con el dato que lo identifica: "El encoder falla con resoluciones que no son
  múltiplo de 16 (1366×768, 0x80004005)".
- **PRs**: `.github/pull_request_template.md`. El título, en español y sin prefijos tipo `feat:`, dice el
  comportamiento nuevo: "Duración máxima de clips según la RAM, hasta 10 min".
- Bullets cortos, sin repetir el diff y sin secciones vacías: la opcional que no aplica se borra.

## Dónde va cada cosa

- Modos del exe, argumentos, candados, títulos de ventana y mensajes a la bandeja: solo en
  `src/app/mode.rs` (`Mode`, `Window`). No escribas `"--settings"` ni `"Local\\Clippy..."` en otro lado.
- Atajos globales: `config::GlobalAction` (ids, etiqueta, tecla). Atajos del editor: `editor/shortcuts.rs`.
- Qué cuenta como juego: solo `src/record/game.rs` (lo usan el modo "Solo juegos" y el canal de audio).
- `config.json` lo escriben la ventana de ajustes y la bandeja; `editor.json` solo el editor;
  `recorder.json` (con qué graba y por qué no puede, `app/status.rs`) solo la bandeja.
- Código que solo sirve para probar va detrás de `#[cfg(feature = "devtools")]`.

## Interfaz y movimiento

Criterios de las skills de Emil Kowalski (`emil-design-eng` y las de animación, instaladas aparte:
`break-ui`, `gestures`, `animation-performance`; no van en el repo). Están escritas para la web: traducilas a egui.
Antes de tocar la interfaz, leé `emil-design-eng`; para revisar, `review-animations`.

- **Colores, duraciones, curvas y widgets solo en `ui_kit/theme.rs`.** Usá `T_PRESS`, `T_TOGGLE`,
  `T_MODAL`, `T_FADE`, `T_SETTLE`, `ease_out`, `ease_in_out`. No escribas otra duración ni otra curva.
- **Nada que se dispare con el teclado se anima** (atajos, deshacer, Esc, F1, Ctrl+E): se usa decenas de
  veces por día y tiene que sentirse inmediato. Lo que cambia desde afuera (bandeja, otro clip) tampoco.
- **Todo lo que se toca con el mouse responde al bajar el botón**: `press` + `paint_pressed`
  (`PRESS_DEPTH` 0.97, `PRESS_DEPTH_ICON` 0.95). Los botones de `theme.rs` ya lo hacen.
- **Sin fundidos de hover** ni animaciones decorativas en lo que se usa seguido. El cabezal y los datos
  de la línea de tiempo nunca se suavizan: siguen al mouse 1 a 1.
- **Respetá `reduced_motion()`** (Windows con "Efectos de animación" apagado): sin movimiento ni escala;
  los fundidos de opacidad quedan.
- **Ninguna animación ni ventana repinta en reposo.** Pedí repintado solo mientras algo se mueve
  (`animate_bool_*` ya lo hace) y nunca `request_repaint()` en cada cuadro "por las dudas".
- **Datos de verdad, no de ejemplo**: nombres y rutas largas se cortan con "…" y muestran el texto completo
  al pasar el mouse; tiempos de más de una hora (`fmt_time`, `fmt_clock`), tamaños (`fmt_bytes`), cantidades
  (`fmt_count`) y plurales bien escritos ("1 clip", "hace 1 semana").
- **Jerarquía**: lo principal de cada pantalla arriba y en `hero_card`; un solo botón `Kind::Primary` visible
  por pantalla.

## Estructura del código

Seguí la skill `untangle-codebase`: lo que cambia junto va junto (una sola definición de cada
valor, texto o regla), nada de casos especiales para un solo valor ni envoltorios que no agregan nada,
y nada de código muerto.

## Probar

```bash
cargo clippy --features devtools -- -D warnings   # chequeo rápido, el mismo que corre el CI
cargo build --profile fast --features devtools    # para probar: unos segundos por cambio
APPDATA=<carpeta>/datos target/fast/clippy.exe --test 8 <carpeta> game
```

Compilá siempre en el `target/` de tu worktree con estos perfiles (el primer build tarda ~1,5 min). Un
`--target-dir` o `build-dir` compartido entre worktrees mezcla los exe: uno termina probando el código de otro.

No hay tests automáticos. Varios agentes prueban a la vez, cada uno en su worktree, así que cada prueba
corre aislada y sin ventanas:

- **Grabación**: `--test` con `APPDATA` apuntando a una carpeta propia. Así el `clippy.log` y el
  `config.json` (los de fábrica) son solo de esa corrida.
- **Editor**: `--test-export`, `--test-frame`, `--test-strip`, `--test-covers` (ver `editor/devtools.rs`).
- **De a uno por vez**, porque tocan cosas de toda la PC: `--settings` y `--editor` (candado de instancia
  única y título de ventana: chocan con otro agente o con el Clippy del usuario), `CLIPPY_TEST_MODE` (cambia
  la resolución del monitor) y medir FPS o CPU. Abrí ventanas solo si el cambio toca la interfaz, al final.
