//! 3dsrecomp, a static recompiler for 3DS titles, whose code runs in any
//! emulator that implements recomp.h, Zakuro being the one it was made for.
//!
//! analyze reports how much of a title's code a generic pass can discover
//! on its own, build turns what it found into C and compiles it into a
//! library the emulator can load, port makes a program of the title with
//! that code linked in, and verify, built with the verify feature, checks
//! the library against Zakuro's interpreter.

use std::path::{Path, PathBuf};
use std::process::exit;

use recomp3ds::build::{self, Event};
use recomp3ds::discover::{self, Analysis, Byte, Mode, Program, Source};
use recomp3ds::rom::Title;
use recomp3ds::{compile, port, Mods};
#[cfg(feature = "verify")]
use recomp3ds::{abi, codegen, image, imported, module_files, rom, static_module, verify};

const USAGE: &str = "\
usage, 3dsrecomp analyze <rom>
       3dsrecomp build <rom> [<dir>] [--overrides <file or dir>]
       3dsrecomp port <rom> <dir> [--overrides <file or dir>] [--name <name>] [--zakuro <checkout>]
       3dsrecomp verify <rom> <library> [count]
       3dsrecomp hints <rom> [<hints file>]
       3dsrecomp symbols <rom> <dir>
each takes --decomp <dir>, code written apart from the game (decomp.bin and
decomp.txt) that replaces some of its functions, see src/decomp.rs, and
--mods <dir>, a mod or an unpacked update laid out the way
Luma3DS has them, code.bin (decompressed, or in exefs/), exheader.bin and
romfs/, read in place of the game's own code and modules. build and port
take --hot <file>, the heat Zakuro --heat wrote, to build only the
functions the game spends nearly all its time in, in a fraction of the time.

symbols writes, for each program, the names the title's modules and its
static.crs export and the functions the analysis finds, for a disassembler.

build without a dir works in the cache and installs the library where
Zakuro finds it on its own. hints prints the game's section for hints.txt,
from the hints Zakuro wrote next to its library or the file given.";

/// what the options on the command line ask for.
#[derive(Default)]
struct Options {
    /// C files of functions written by hand, see docs/overrides.md.
    overrides: Option<PathBuf>,
    /// what port calls the program.
    name: Option<String>,
    /// a Zakuro checkout for port to build against.
    zakuro: Option<PathBuf>,
    /// what a mod's folder changes in the title.
    mods: Option<ModDir>,
    /// code written apart from the title, linked at an address of its own.
    decomp: Option<recomp3ds::decomp::Decomp>,
    /// Zakuro's heat, to build only the hottest functions.
    hot: Option<PathBuf>,
}

impl Options {
    /// what the mod changes, files reading its RomFS files.
    fn mods<'a>(&'a self, files: recomp3ds::Files<'a>) -> Mods<'a> {
        let decomp = self.decomp.as_ref();
        let Some(dir) = &self.mods else { return Mods { decomp, ..Mods::default() } };
        Mods { code: dir.code.as_deref(), exheader: dir.exheader.as_deref(), romfs: dir.romfs.is_some().then_some(files), decomp }
    }

    /// the RomFS file at path the mod gives, none to keep the title's.
    fn mod_file(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.mods.as_ref()?.romfs.as_ref()?.join(path)).ok()
    }
}

/// a mod, or an update unpacked, in a folder laid out the way Luma3DS reads
/// them: code.bin, decompressed, in it or in exefs/, exheader.bin, and the
/// RomFS files it replaces under romfs/.
struct ModDir {
    code: Option<Vec<u8>>,
    exheader: Option<Vec<u8>>,
    romfs: Option<PathBuf>,
}

impl ModDir {
    fn read(dir: &Path) -> ModDir {
        let code = ["code.bin", "exefs/code.bin"].into_iter().find_map(|name| std::fs::read(dir.join(name)).ok());
        let exheader = std::fs::read(dir.join("exheader.bin")).ok();
        let romfs = Some(dir.join("romfs")).filter(|romfs| romfs.is_dir());
        if code.is_none() && exheader.is_none() && romfs.is_none() {
            eprintln!("{} has no code.bin, exheader.bin or romfs", dir.display());
            exit(1);
        }
        ModDir { code, exheader, romfs }
    }
}

fn main() {
    let mut words = Vec::new();
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next().unwrap_or_else(|| {
                eprintln!("{arg} needs a value");
                exit(2);
            })
        };
        match arg.as_str() {
            "--overrides" => options.overrides = Some(value().into()),
            "--name" => options.name = Some(value()),
            "--zakuro" => options.zakuro = Some(value().into()),
            "--mods" => options.mods = Some(ModDir::read(Path::new(&value()))),
            "--decomp" => {
                options.decomp = Some(recomp3ds::decomp::Decomp::read(Path::new(&value())).unwrap_or_else(|error| {
                    eprintln!("{error}");
                    exit(2);
                }))
            }
            "--hot" => options.hot = Some(value().into()),
            other if other.starts_with("--") => {
                eprintln!("unknown option {other}\n{USAGE}");
                exit(2);
            }
            _ => words.push(arg),
        }
    }
    match words.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["analyze", rom] => analyze(rom, &options),
        ["build", rom] => build(rom, None, &options),
        ["build", rom, dir] => build(rom, Some(Path::new(dir)), &options),
        ["port", rom, dir] => port(rom, Path::new(dir), &options),
        ["symbols", rom, dir] => symbols(rom, Path::new(dir), &options),
        ["hints", rom] => hints(rom, None, &options),
        ["hints", rom, file] => hints(rom, Some(Path::new(file)), &options),
        #[cfg(feature = "verify")]
        ["verify", rom, library] => check(rom, Path::new(library), 500, &options),
        #[cfg(feature = "verify")]
        ["verify", rom, library, count] => check(rom, Path::new(library), count.parse().unwrap_or(500), &options),
        #[cfg(not(feature = "verify"))]
        ["verify", ..] => {
            eprintln!("this build has no verify, build it with --features verify");
            exit(2);
        }
        _ => {
            eprintln!("{USAGE}");
            exit(2);
        }
    }
}

/// the title and its programs, the executable first and then its modules.
fn load(path: &str, mods: Mods) -> (Title, Vec<(String, Program)>) {
    let title = Title::load(path).unwrap_or_else(|error| {
        eprintln!("could not load {path}, {error}");
        exit(1);
    });
    let programs = recomp3ds::programs(&title, mods).unwrap_or_else(|error| {
        eprintln!("could not read the code, {error}");
        exit(1);
    });
    (title, programs)
}

/// prints the section of hints.txt for the game at path, the places in the
/// hints Zakuro wrote next to its library, or in file, that start functions
/// the analysis does not find, along with those the list has already.
fn hints(path: &str, file: Option<&Path>, options: &Options) {
    let files = |path: &str| options.mod_file(path);
    let (title, programs) = load(path, options.mods(&files));
    let Some((_, program)) = programs.iter().find(|(name, _)| name == "executable") else {
        eprintln!("{path} has no executable");
        exit(1);
    };
    let mut hinted = match file {
        Some(file) => match std::fs::read_to_string(file) {
            Ok(text) => build::read_hints(&text),
            Err(error) => {
                eprintln!("could not read {}, {error}", file.display());
                exit(1);
            }
        },
        None => build::hints(title.program_id()),
    };
    let hash = build::code_hash(program);
    hinted.extend(build::bundled(title.program_id(), hash));
    let text = program.text.base..program.text.end();
    hinted.retain(|&address| text.contains(&(address & !1)));
    hinted.sort_unstable();
    hinted.dedup();
    let new = build::new_functions(&discover::analyze(program), &hinted);
    println!("= {:016X} {hash:016X}", title.program_id());
    for address in new {
        println!("{address:08X}");
    }
}

/// writes dir/<program>.txt for each program: a line for each name its CRO,
/// or static.crs for the executable, exports, and one for each function the
/// analysis finds that has none, f_ or t_ and its address. a line is
/// "name address kind", kind f for an ARM function, t for a Thumb one and l
/// for data. the executable's addresses are where it is loaded, a module's
/// are offsets in its file, as overrides and Zakuro's heat name them.
fn symbols(path: &str, dir: &Path, options: &Options) {
    let files = |path: &str| options.mod_file(path);
    let mods = options.mods(&files);
    let (title, programs) = load(path, mods);
    let modules = recomp3ds::module_files(&title, mods);
    let crs = recomp3ds::static_module(&title, mods);
    if let Err(error) = std::fs::create_dir_all(dir) {
        eprintln!("could not create {}, {error}", dir.display());
        exit(1);
    }
    let (mut named, mut found) = (0, 0);
    for (name, program) in &programs {
        let exporter = match name.as_str() {
            "executable" => crs.as_ref(),
            _ => modules.iter().map(|(module, _)| module).find(|module| module.name == *name),
        };
        let analysis = discover::analyze(program);
        let thumb = |at: u32| at & 1 != 0 || analysis.functions.get(&(at & !1)).is_some_and(|f| f.mode == Mode::Thumb);
        let mut text = format!("# {name}: name address kind (f ARM function, t Thumb function, l data)\n");
        let mut has_name = std::collections::BTreeSet::new();
        for (symbol, tag) in exporter.map(|module| module.exports.as_slice()).unwrap_or_default() {
            let (at, kind) = match exporter.and_then(|module| module.code_address(*tag)) {
                Some(at) => (at & !1, if thumb(at) { 't' } else { 'f' }),
                None => match exporter.and_then(|module| module.resolve(*tag)) {
                    Some(at) => (at, 'l'),
                    None => continue,
                },
            };
            text.push_str(&format!("{symbol} {at:08X} {kind}\n"));
            has_name.insert(at);
            named += 1;
        }
        for (&entry, function) in &analysis.functions {
            if has_name.insert(entry) {
                let kind = if function.mode == Mode::Thumb { 't' } else { 'f' };
                text.push_str(&format!("{kind}_{entry:08X} {entry:08X} {kind}\n"));
                found += 1;
            }
        }
        let file = dir.join(format!("{name}.txt"));
        if let Err(error) = std::fs::write(&file, text) {
            eprintln!("could not write {}, {error}", file.display());
            exit(1);
        }
    }
    println!("{} programs, {named} exported names, {found} functions found without one, in {}", programs.len(), dir.display());
}

/// builds the library in dir, or when there is none in the cache, and then
/// installs it where hosts look for it.
fn build(path: &str, given: Option<&Path>, options: &Options) {
    let files = |path: &str| options.mod_file(path);
    let build_options = build::Options {
        dir: given,
        overrides: options.overrides.as_deref(),
        mods: options.mods(&files),
        hot: options.hot.as_deref(),
        ..build::Options::default()
    };
    if let Err(error) = build::build(Path::new(path), &build_options, &print_event) {
        eprintln!("{error}");
        exit(1);
    }
}

/// prints what a build does, compiling every twentieth of the way, which a
/// frontend running build can follow.
fn print_event(event: Event) {
    match event {
        Event::Generated { files, bytes, overrides } => {
            println!("wrote {files} files, {} MiB of C, {overrides} overrides", bytes >> 20)
        }
        Event::Note(note) => println!("{note}"),
        Event::Compiled { done, total } => {
            if done == total || done * 20 / total != (done - 1) * 20 / total {
                println!("compiled {done} of {total}");
            }
        }
        Event::Built { library, took } => println!("built {} in {took:.1?}", library.display()),
        Event::Installed(path) => println!("installed {}, which Zakuro runs for the game from now on", path.display()),
    }
}

/// a Cargo project at dir that builds the title into a program of its own.
fn port(path: &str, dir: &Path, options: &Options) {
    let files = |path: &str| options.mod_file(path);
    let generated = build::generate(Path::new(path), |_| dir.join("code"), options.overrides.as_deref(), options.mods(&files), options.hot.as_deref(), &print_event)
        .unwrap_or_else(|error| {
            eprintln!("{error}");
            exit(1);
        });
    let (title, code) = (&generated.title, &generated.dir);
    let start = std::time::Instant::now();
    let progress = |done, total| {
        print_event(Event::Compiled { done, total });
        true
    };
    let archived = compile::check(None)
        .and_then(|compiler| compile::objects(&compiler, code, &generated.sources, &progress, false))
        .and_then(|objects| compile::archive(&objects, &code.join("librecomp.a")));
    if let Err(error) = archived {
        eprintln!("{error}");
        exit(1);
    }
    println!("built {} in {:.1?}", code.join("librecomp.a").display(), start.elapsed());

    let name = options.name.clone().unwrap_or_else(|| port::package_name(&title.exheader.title));
    let written = port::write_project(dir, &name, &title.exheader.title, title.program_id(), options.zakuro.as_deref());
    if let Err(error) = written {
        eprintln!("{error}");
        exit(1);
    }
    println!("wrote the project for {name}, build it with cargo build --release in {}", dir.display());
    println!("then run target/release/{name} with the path to the game");
}

/// runs up to count of the recompiled functions of each program against
/// the interpreter, the modules one at a time, each loaded at the same place.
#[cfg(feature = "verify")]
fn check(path: &str, library: &Path, count: usize, options: &Options) {
    let files = |path: &str| options.mod_file(path);
    let mods = options.mods(&files);
    let (title, programs) = load(path, mods);
    let mut library = abi::Library::open(library).unwrap_or_else(|error| {
        eprintln!("could not open {}, {error}", library.display());
        exit(1);
    });
    // the mod's code where its exheader, or else the title's, lays it
    let exheader = mods.exheader.filter(|ex| ex.len() >= 0x400).map(rom::ExHeader::read).unwrap_or_else(|| title.exheader.clone());
    let image = match mods.code {
        Some(code) => image::Image::from_code(&exheader, code),
        None => image::Image::from_title(&title).unwrap_or_else(|error| {
            eprintln!("could not read the code, {error}");
            exit(1);
        }),
    };
    let regions = verify::regions(
        (image.text.base, &image.text.bytes),
        (image.rodata.base, &image.rodata.bytes),
        (image.data.base, &image.data.bytes),
        exheader.bss_size,
    );
    let start = std::time::Instant::now();

    let executable = verify::verify(
        "executable",
        &verify::Memory::new(regions.clone()),
        &library,
        &sample(&discover::analyze(&programs[0].1), 0, count),
    );
    print_report("executable", &executable);

    let mut modules = verify::Report::default();
    let files = module_files(&title, mods);
    let crs = static_module(&title, mods);
    for (module, bytes) in &files {
        let Some(index) = library.module_index(&module.name) else { continue };
        let Some(program) = module.program(bytes, &imported(&files, crs.as_ref(), module)) else { continue };
        let mut memory = regions.clone();
        memory.push(verify::Region {
            base: verify::MODULE_BASE,
            bytes: module.image(bytes, verify::MODULE_BASE, verify::IMPORT_STUB),
            writable: true,
        });
        library.place(index, verify::MODULE_BASE);
        let report = verify::verify(
            &module.name,
            &verify::Memory::new(memory),
            &library,
            &sample(&discover::analyze(&program), verify::MODULE_BASE, count),
        );
        library.place(index, 0);
        modules.add(&report);
    }
    print_report("modules", &modules);
    println!("took {:.1?}", start.elapsed());
}

/// up to count of the functions that became C, spread over the program, as
/// addresses at base with bit 0 set for Thumb.
#[cfg(feature = "verify")]
fn sample(analysis: &Analysis, base: u32, count: usize) -> Vec<u32> {
    let functions: Vec<u32> = analysis
        .functions
        .iter()
        .filter(|&(&entry, f)| codegen::recompiles(entry, f))
        .map(|(&entry, f)| (base + entry) | (f.mode == Mode::Thumb) as u32)
        .collect();
    // VERIFY_ONLY=address checks that one function alone
    if let Some(only) = std::env::var("VERIFY_ONLY").ok().and_then(|v| u32::from_str_radix(&v, 16).ok()) {
        return functions.into_iter().filter(|&f| f == only).collect();
    }
    let step = (functions.len() / count.max(1)).max(1);
    functions.into_iter().step_by(step).take(count).collect()
}

#[cfg(feature = "verify")]
fn print_report(name: &str, report: &verify::Report) {
    println!(
        "{name:<11} {} functions, {} returned, {} reached an svc, {} stuck, {} stopped otherwise, {} mismatched",
        report.tested, report.returned, report.svc, report.stuck, report.other, report.mismatched
    );
    println!(
        "{:<11} instructions {} recompiled, {} through the fallback, {} interpreted alone",
        "", report.native, report.fallbacks, report.interpreted
    );
}

fn analyze(path: &str, options: &Options) {
    let files = |path: &str| options.mod_file(path);
    let (title, mut programs) = load(path, options.mods(&files));
    let module_count = programs.len() - 1;
    let start = std::time::Instant::now();
    let mut analyses: Vec<Analysis> = programs.iter().map(|(_, program)| discover::analyze(program)).collect();
    let functions = build::apply_hints(&title, &mut programs, &mut analyses);
    let elapsed = start.elapsed();

    println!("title       {}, {} modules", title.exheader.title, module_count);
    if functions > 0 {
        println!("hints       {functions} new functions, from where Zakuro interpreted");
    }
    println!();
    println!("                  KiB  functions   code  literals  unreached  indirect  switches    svc  dead ends");
    let executable = Totals::of(&analyses[0]);
    let modules = analyses[1..].iter().map(Totals::of).fold(Totals::default(), Totals::add);
    executable.print("executable");
    modules.print("modules");
    executable.add(modules).print("all");
    println!();

    let functions = || analyses.iter().flat_map(|analysis| analysis.functions.values());
    let from = |source: Source| functions().filter(|f| f.source == source).count();
    println!(
        "found by    {} calls, {} pointers, {} relocations, {} exports, {} imports, {} scanned, {} hinted, {} entry",
        from(Source::Call),
        from(Source::Pointer),
        from(Source::Relocation),
        from(Source::Export),
        from(Source::Import),
        from(Source::Scan),
        from(Source::Hint),
        from(Source::Entry)
    );
    let thumb = functions().filter(|f| f.mode == Mode::Thumb).count();
    let instructions: usize = functions().map(|f| f.instructions.len()).sum();
    println!("thumb       {thumb} functions");
    println!("decoded     {instructions} instructions, shared code once per function");
    // a function far bigger than the rest takes the compiler most of the
    // build, alone
    let mut largest: Vec<(usize, u32)> = analyses
        .iter()
        .flat_map(|analysis| analysis.functions.iter().map(|(&entry, f)| (f.instructions.len(), entry)))
        .collect();
    largest.sort_unstable_by(|a, b| b.cmp(a));
    let largest: Vec<String> = largest.iter().take(3).map(|(size, entry)| format!("{size} at {entry:08X}")).collect();
    println!("largest     {}", largest.join(", "));
    println!("analysis    {elapsed:.2?}");
    println!();

    println!("largest unreached runs");
    let mut gaps: Vec<_> = programs
        .iter()
        .zip(&analyses)
        .flat_map(|((name, program), analysis)| {
            analysis.largest_gaps(program.text.base, 8).into_iter().map(move |(start, length)| (name, start, length))
        })
        .collect();
    gaps.sort_by_key(|gap| std::cmp::Reverse(gap.2));
    for (name, start, length) in gaps.into_iter().take(10) {
        println!("  {name:<28} 0x{start:08X}  {length} bytes");
    }
}

/// the numbers the report shows for a program, or for several added up.
#[derive(Default, Clone, Copy)]
struct Totals {
    text: usize,
    functions: usize,
    code: usize,
    literal: usize,
    indirect: usize,
    switches: usize,
    svc: usize,
    dead_ends: usize,
}

impl Totals {
    fn of(analysis: &Analysis) -> Totals {
        Totals {
            text: analysis.map.len(),
            functions: analysis.functions.len(),
            code: analysis.count(Byte::Code),
            literal: analysis.count(Byte::Literal),
            indirect: analysis.indirect_sites,
            switches: analysis.jump_tables,
            svc: analysis.svc_sites,
            dead_ends: analysis.dead_ends,
        }
    }

    fn add(self, other: Totals) -> Totals {
        Totals {
            text: self.text + other.text,
            functions: self.functions + other.functions,
            code: self.code + other.code,
            literal: self.literal + other.literal,
            indirect: self.indirect + other.indirect,
            switches: self.switches + other.switches,
            svc: self.svc + other.svc,
            dead_ends: self.dead_ends + other.dead_ends,
        }
    }

    fn print(&self, name: &str) {
        let percent = |bytes: usize| bytes as f64 * 100.0 / self.text.max(1) as f64;
        let unreached = self.text - self.code - self.literal;
        println!(
            "{name:<12} {:>9} {:>10} {:>5.1}% {:>8.1}% {:>9.1}% {:>9} {:>9} {:>6} {:>10}",
            self.text / 1024,
            self.functions,
            percent(self.code),
            percent(self.literal),
            percent(unreached),
            self.indirect,
            self.switches,
            self.svc,
            self.dead_ends
        );
    }
}
