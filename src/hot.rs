//! building first only the functions a game spends nearly all its time in,
//! from the heat Zakuro writes with --heat: samples by program and address,
//! a module's by its offset from where its file starts. the rest stays in
//! the interpreter until a full build, which a host can run in the
//! background while the game is played.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::discover::{Analysis, Program};

/// how much of the samples the functions built first cover.
pub const SHARE: f64 = 0.999;

/// samples by program name, "executable" or a module's, and address.
pub struct Heat {
    samples: HashMap<String, HashMap<u32, u64>>,
}

impl Heat {
    pub fn load(path: &Path) -> Result<Heat, String> {
        let text = std::fs::read_to_string(path).map_err(|error| format!("could not read {}, {error}", path.display()))?;
        Ok(Heat::parse(&text))
    }

    pub fn parse(text: &str) -> Heat {
        let mut samples: HashMap<String, HashMap<u32, u64>> = HashMap::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [program, at, hits] = fields[..] else { continue };
            let (Ok(at), Ok(hits)) = (u32::from_str_radix(at, 16), hits.parse::<u64>()) else { continue };
            // the Thumb bit says nothing of which function holds the place
            *samples.entry(program.to_owned()).or_default().entry(at & !1).or_insert(0) += hits;
        }
        Heat { samples }
    }
}

/// what select kept, for telling.
pub struct Selection {
    /// for each program, the entries of the functions to build.
    pub keep: Vec<BTreeSet<u32>>,
    pub functions: usize,
    pub of: usize,
    /// the share of the samples in the functions found that they cover.
    pub covered: f64,
}

/// the functions of each program, hottest first, until they hold share of
/// the samples that fall in any function. a place several functions share
/// counts for each of them.
pub fn select(programs: &[(String, Program)], analyses: &[Analysis], heat: &Heat, share: f64) -> Selection {
    let mut ranked: Vec<(u64, usize, u32)> = Vec::new();
    for (index, ((name, _), analysis)) in programs.iter().zip(analyses).enumerate() {
        let Some(samples) = heat.samples.get(name) else { continue };
        for (&entry, function) in &analysis.functions {
            let hits: u64 = function.instructions.iter().filter_map(|at| samples.get(at)).sum();
            if hits > 0 {
                ranked.push((hits, index, entry));
            }
        }
    }
    ranked.sort_unstable_by(|a, b| b.cmp(a));
    let total: u64 = ranked.iter().map(|&(hits, ..)| hits).sum();
    let mut keep = vec![BTreeSet::new(); programs.len()];
    let mut taken = 0;
    for &(hits, index, entry) in &ranked {
        if taken as f64 >= share * total as f64 {
            break;
        }
        keep[index].insert(entry);
        taken += hits;
    }
    let functions = keep.iter().map(BTreeSet::len).sum();
    let of = analyses.iter().map(|analysis| analysis.functions.len()).sum();
    Selection { keep, functions, of, covered: if total == 0 { 0.0 } else { taken as f64 / total as f64 } }
}

/// leaves in each analysis only the functions keep names, and those in
/// also, the ones overrides replace, which have to be generated for them.
pub fn prune(analyses: &mut [Analysis], keep: &[BTreeSet<u32>], also: &BTreeMap<usize, BTreeSet<u32>>) {
    for (index, analysis) in analyses.iter_mut().enumerate() {
        let none = BTreeSet::new();
        let extra = also.get(&index).unwrap_or(&none);
        analysis.functions.retain(|entry, function| {
            keep[index].contains(entry) || function.labels.iter().any(|label| extra.contains(label))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{self, Source};
    use crate::image::Segment;

    /// three functions, the first called from the others, each returning.
    fn program() -> Program {
        let words = [0xEB00_0002u32, 0xE12F_FF1E, 0xEAFF_FFFE, 0xE12F_FF1E, 0xE12F_FF1E];
        Program {
            text: Segment { base: 0x0010_0000, bytes: words.iter().flat_map(|w| w.to_le_bytes()).collect() },
            seeds: vec![(0x0010_0000, Source::Entry), (0x0010_0008, Source::Entry)],
            slots: None,
        }
    }

    /// the hottest functions come first, until they cover the share, and a
    /// place in a module counts for the module's function, not the
    /// executable's at the same address.
    #[test]
    fn the_hottest_functions_cover_the_share() {
        let programs = vec![("executable".to_owned(), program()), ("Battle".to_owned(), program())];
        let analyses: Vec<Analysis> = programs.iter().map(|(_, program)| discover::analyze(program)).collect();
        let entries: Vec<u32> = analyses[0].functions.keys().copied().collect();
        assert!(entries.len() >= 2, "{entries:X?}");
        let heat = Heat::parse(&format!(
            "# a note\nexecutable {:08X} 900\nexecutable {:08X} 1\nBattle {:08X} 99\n",
            entries[0], entries[1], entries[1]
        ));
        let all = select(&programs, &analyses, &heat, 1.0);
        assert_eq!(all.keep[0], [entries[0], entries[1]].into());
        assert_eq!(all.keep[1], [entries[1]].into());
        let most = select(&programs, &analyses, &heat, 0.9);
        assert_eq!(most.keep[0], [entries[0]].into());
        assert!(most.keep[1].is_empty());
    }
}
