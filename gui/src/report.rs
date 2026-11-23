//! what analyze finds, kept as numbers for the window to lay out, same as the cli's analyze prints

use std::time::Duration;

use recomp3ds::build;
use recomp3ds::discover::{self, Analysis, Byte, Mode, Program, Source};
use recomp3ds::rom::Title;

/// the numbers for a program, or several summed
#[derive(Default, Clone, Copy)]
pub struct Totals {
    pub text: usize,
    pub functions: usize,
    pub code: usize,
    pub literal: usize,
    pub indirect: usize,
    pub switches: usize,
    pub svc: usize,
    pub dead_ends: usize,
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

    pub fn unreached(&self) -> usize {
        self.text - self.code - self.literal
    }

    /// a share of the code segment, 0 to 1
    pub fn share(&self, bytes: usize) -> f32 {
        bytes as f32 / self.text.max(1) as f32
    }
}

pub struct Report {
    pub modules: usize,
    /// funcs the hints added
    pub hinted: usize,
    pub executable: Totals,
    pub module_totals: Totals,
    /// how many funcs each way of finding them found
    pub found_by: Vec<(&'static str, usize)>,
    pub thumb: usize,
    pub instructions: usize,
    /// (instructions, entry)
    pub largest: Vec<(usize, u32)>,
    pub took: Duration,
    /// (program, start, bytes)
    pub gaps: Vec<(String, u32, usize)>,
}

impl Report {
    pub fn new(title: &Title, mut programs: Vec<(String, Program)>) -> Report {
        let start = std::time::Instant::now();
        let mut analyses: Vec<Analysis> = programs.iter().map(|(_, program)| discover::analyze(program)).collect();
        let hinted = build::apply_hints(title, &mut programs, &mut analyses);
        let took = start.elapsed();

        let functions = || analyses.iter().flat_map(|analysis| analysis.functions.values());
        let from = |source: Source| functions().filter(|f| f.source == source).count();
        let found_by = vec![
            ("calls", from(Source::Call)),
            ("pointers", from(Source::Pointer)),
            ("relocations", from(Source::Relocation)),
            ("exports", from(Source::Export)),
            ("imports", from(Source::Import)),
            ("scanned", from(Source::Scan)),
            ("hinted", from(Source::Hint)),
            ("entry", from(Source::Entry)),
        ];
        let mut largest: Vec<(usize, u32)> = analyses
            .iter()
            .flat_map(|analysis| analysis.functions.iter().map(|(&entry, f)| (f.instructions.len(), entry)))
            .collect();
        largest.sort_unstable_by(|a, b| b.cmp(a));
        largest.truncate(3);
        let mut gaps: Vec<(String, u32, usize)> = programs
            .iter()
            .zip(&analyses)
            .flat_map(|((name, program), analysis)| {
                analysis.largest_gaps(program.text.base, 8).into_iter().map(move |(start, length)| (name.clone(), start, length))
            })
            .collect();
        gaps.sort_by_key(|gap| std::cmp::Reverse(gap.2));
        gaps.truncate(10);

        Report {
            modules: programs.len() - 1,
            hinted,
            executable: Totals::of(&analyses[0]),
            module_totals: analyses[1..].iter().map(Totals::of).fold(Totals::default(), Totals::add),
            thumb: functions().filter(|f| f.mode == Mode::Thumb).count(),
            instructions: functions().map(|f| f.instructions.len()).sum(),
            found_by,
            largest,
            took,
            gaps,
        }
    }

    pub fn all(&self) -> Totals {
        self.executable.add(self.module_totals)
    }
}
