//! Turning observed usage into a request and limit suggestion.
//!
//! The arithmetic is the easy part. The hard part is not overstating what a
//! short observation can support: a workload watched for ten minutes has not
//! been seen at month-end, during a backup, or under the traffic it gets on a
//! Monday. Every recommendation therefore carries how long it watched and how
//! many samples it has, and says plainly when that is not enough to act on.

use serde::{Deserialize, Serialize};

/// Headroom above observed peak for a memory limit.
///
/// Memory is not compressible: exceeding the limit is an OOM kill, not a
/// slowdown. The margin is deliberately generous.
const MEMORY_LIMIT_HEADROOM: f64 = 1.4;

/// Headroom above the p95 for a memory request.
const MEMORY_REQUEST_HEADROOM: f64 = 1.15;

/// Headroom above the p95 for a CPU request.
const CPU_REQUEST_HEADROOM: f64 = 1.2;

/// Below this many samples the numbers are not worth acting on.
const MINIMUM_SAMPLES: usize = 8;

/// An observation window shorter than this cannot have seen a daily cycle.
const CONFIDENT_WINDOW_SECONDS: i64 = 6 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    /// Enough samples over a long enough window to act on.
    Reasonable,
    /// Usable as a starting point, but the window is short.
    Indicative,
    /// Too little data. Shown, but never presented as a recommendation.
    Insufficient,
}

/// One container's usage, and what it suggests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    pub container: String,

    pub samples: usize,
    /// Seconds between the first and last sample.
    pub window_seconds: i64,
    pub confidence: Confidence,

    /// Cores.
    pub cpu_p95: f64,
    pub cpu_max: f64,
    /// Bytes.
    pub memory_p95: f64,
    pub memory_max: f64,

    pub current_cpu_request: f64,
    pub current_cpu_limit: f64,
    pub current_memory_request: f64,
    pub current_memory_limit: f64,

    pub recommended_cpu_request: f64,
    pub recommended_memory_request: f64,
    pub recommended_memory_limit: f64,
    /// `None` on purpose — see `notes`.
    pub recommended_cpu_limit: Option<f64>,

    /// Plain-language observations, including why a value is what it is.
    pub notes: Vec<String>,
}

/// Percentile of a sorted-in-place copy. Nearest-rank, which needs no
/// interpolation and cannot invent a value that was never observed.
fn percentile(values: &[f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((sorted.len() as f64) * fraction).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Round a CPU figure to a readable millicore value.
fn round_cpu(value: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    // 10m granularity below one core, 50m above: nobody wants `137m`.
    let step = if value < 1.0 { 0.01 } else { 0.05 };
    (value / step).ceil() * step
}

/// Round memory up to a whole number of mebibytes.
fn round_memory(value: f64) -> f64 {
    if value <= 0.0 {
        return 0.0;
    }
    const MIB: f64 = 1024.0 * 1024.0;
    (value / MIB).ceil() * MIB
}

/// What one container's samples suggest.
///
/// `samples` are `(cpu_cores, memory_bytes)` pairs in observation order;
/// `window_seconds` is the span they cover.
pub fn build(
    container: &str,
    samples: &[(f64, f64)],
    window_seconds: i64,
    current_cpu_request: f64,
    current_cpu_limit: f64,
    current_memory_request: f64,
    current_memory_limit: f64,
) -> Recommendation {
    let cpu: Vec<f64> = samples.iter().map(|(cpu, _)| *cpu).collect();
    let memory: Vec<f64> = samples.iter().map(|(_, memory)| *memory).collect();

    let cpu_p95 = percentile(&cpu, 0.95);
    let cpu_max = cpu.iter().copied().fold(0.0, f64::max);
    let memory_p95 = percentile(&memory, 0.95);
    let memory_max = memory.iter().copied().fold(0.0, f64::max);

    let confidence = if samples.len() < MINIMUM_SAMPLES {
        Confidence::Insufficient
    } else if window_seconds < CONFIDENT_WINDOW_SECONDS {
        Confidence::Indicative
    } else {
        Confidence::Reasonable
    };

    let mut notes = Vec::new();

    match confidence {
        Confidence::Insufficient => notes.push(format!(
            "Only {} samples so far — too few to suggest anything. Leave this panel open and \
             come back.",
            samples.len()
        )),
        Confidence::Indicative => notes.push(format!(
            "Based on {} samples over {}. That has not seen a daily peak, a backup window or \
             month-end, so treat it as a starting point rather than an answer.",
            samples.len(),
            humanise(window_seconds)
        )),
        Confidence::Reasonable => notes.push(format!(
            "Based on {} samples over {}.",
            samples.len(),
            humanise(window_seconds)
        )),
    }

    let recommended_cpu_request = round_cpu(cpu_p95 * CPU_REQUEST_HEADROOM);
    let recommended_memory_request = round_memory(memory_p95 * MEMORY_REQUEST_HEADROOM);
    let recommended_memory_limit = round_memory(memory_max * MEMORY_LIMIT_HEADROOM);

    // A CPU limit throttles rather than kills, and throttling a latency-
    // sensitive service to save a resource nobody is short of is a common own
    // goal. Say so instead of emitting a number.
    notes.push(
        "No CPU limit is suggested: exceeding one throttles the container rather than freeing \
         capacity, and a correct request is what the scheduler actually uses. Set one only where \
         a noisy neighbour must be contained."
            .into(),
    );

    if current_memory_limit > 0.0 && memory_max > current_memory_limit * 0.9 {
        notes.push(format!(
            "Peak memory reached {:.0}% of the current limit. Exceeding it is an OOM kill, not a \
             slowdown.",
            memory_max / current_memory_limit * 100.0
        ));
    }
    if current_cpu_request > 0.0 && cpu_p95 < current_cpu_request * 0.3 {
        notes.push(format!(
            "The CPU request is roughly {:.0}× the observed p95, so that much capacity is \
             reserved and unused on whichever node this lands on.",
            current_cpu_request / cpu_p95.max(0.0001)
        ));
    }
    // Lowering a memory limit is the single change here most likely to cause an
    // outage: the workload keeps running until the day it needs the memory, and
    // then it is killed rather than slowed. Saying "suggested 35Mi" beside a
    // current 512Mi without this reads as an invitation.
    if current_memory_limit > 0.0 && recommended_memory_limit < current_memory_limit * 0.7 {
        notes.push(format!(
            "This suggests cutting the memory limit from {} to {} — a {:.0}% reduction. Memory \
             limits are enforced by killing the container, so only lower one after observing a \
             window that includes the workload's real peak.",
            format_bytes(current_memory_limit),
            format_bytes(recommended_memory_limit),
            (1.0 - recommended_memory_limit / current_memory_limit) * 100.0
        ));
    }

    if current_memory_request == 0.0 {
        notes.push(
            "No memory request is set, so the scheduler places this pod blind and evicts it \
             first under pressure."
                .into(),
        );
    }

    Recommendation {
        container: container.to_string(),
        samples: samples.len(),
        window_seconds,
        confidence,
        cpu_p95,
        cpu_max,
        memory_p95,
        memory_max,
        current_cpu_request,
        current_cpu_limit,
        current_memory_request,
        current_memory_limit,
        recommended_cpu_request,
        recommended_memory_request,
        recommended_memory_limit,
        recommended_cpu_limit: None,
        notes,
    }
}

/// Mebibytes, the unit these values are written in.
fn format_bytes(value: f64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if value >= 1024.0 * MIB {
        format!("{:.1}Gi", value / 1024.0 / MIB)
    } else {
        format!("{:.0}Mi", value / MIB)
    }
}

fn humanise(seconds: i64) -> String {
    match seconds {
        s if s < 90 => format!("{s} seconds"),
        s if s < 5400 => format!("{} minutes", s / 60),
        s if s < 172_800 => format!("{} hours", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: f64 = 1024.0 * 1024.0;

    fn samples(count: usize, cpu: f64, memory: f64) -> Vec<(f64, f64)> {
        (0..count).map(|_| (cpu, memory)).collect()
    }

    #[test]
    fn percentile_uses_an_observed_value() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 100.0];
        // Nearest-rank, so the answer is a number that actually occurred.
        assert_eq!(percentile(&values, 0.95), 100.0);
        assert_eq!(percentile(&values, 0.5), 3.0);
        assert_eq!(percentile(&[], 0.95), 0.0);
    }

    /// Too little data must never be dressed up as advice.
    #[test]
    fn a_handful_of_samples_is_reported_as_insufficient() {
        let recommendation = build("app", &samples(3, 0.1, 100.0 * MIB), 45, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(recommendation.confidence, Confidence::Insufficient);
        assert!(recommendation.notes[0].contains("too few"));
    }

    /// A short window is usable but must say what it has not seen.
    #[test]
    fn a_short_window_is_indicative_not_reasonable() {
        let recommendation = build(
            "app",
            &samples(40, 0.1, 100.0 * MIB),
            600,
            0.0,
            0.0,
            0.0,
            0.0,
        );
        assert_eq!(recommendation.confidence, Confidence::Indicative);
        assert!(
            recommendation.notes[0].contains("starting point"),
            "{:?}",
            recommendation.notes
        );
    }

    #[test]
    fn a_long_window_with_enough_samples_is_reasonable() {
        let recommendation = build(
            "app",
            &samples(400, 0.1, 100.0 * MIB),
            8 * 3600,
            0.0,
            0.0,
            0.0,
            0.0,
        );
        assert_eq!(recommendation.confidence, Confidence::Reasonable);
    }

    /// Requests sit above the p95, and the memory limit above the peak —
    /// never below what was actually observed.
    #[test]
    fn recommendations_never_fall_below_observed_usage() {
        let mut usage = samples(50, 0.2, 200.0 * MIB);
        usage.push((0.9, 500.0 * MIB));

        let recommendation = build("app", &usage, 7200, 0.0, 0.0, 0.0, 0.0);
        assert!(recommendation.recommended_cpu_request >= recommendation.cpu_p95);
        assert!(recommendation.recommended_memory_request >= recommendation.memory_p95);
        assert!(
            recommendation.recommended_memory_limit >= recommendation.memory_max,
            "a limit below the observed peak would guarantee an OOM kill"
        );
    }

    #[test]
    fn cpu_limits_are_deliberately_not_suggested() {
        let recommendation = build(
            "app",
            &samples(50, 0.2, 200.0 * MIB),
            7200,
            0.5,
            1.0,
            0.0,
            0.0,
        );
        assert!(recommendation.recommended_cpu_limit.is_none());
        assert!(
            recommendation
                .notes
                .iter()
                .any(|note| note.contains("throttles")),
            "the reason must be stated, not just the omission"
        );
    }

    #[test]
    fn nearing_the_memory_limit_is_called_out() {
        let usage = samples(50, 0.1, 95.0 * MIB);
        let recommendation = build("app", &usage, 7200, 0.0, 0.0, 0.0, 100.0 * MIB);
        assert!(
            recommendation
                .notes
                .iter()
                .any(|note| note.contains("OOM kill")),
            "{:?}",
            recommendation.notes
        );
    }

    #[test]
    fn a_wildly_oversized_request_is_called_out() {
        let usage = samples(50, 0.05, 50.0 * MIB);
        let recommendation = build("app", &usage, 7200, 2.0, 0.0, 0.0, 0.0);
        assert!(
            recommendation
                .notes
                .iter()
                .any(|note| note.contains("reserved and unused")),
            "{:?}",
            recommendation.notes
        );
    }

    /// The dangerous direction. A workload idle during observation would
    /// otherwise be handed a limit that kills it under real load.
    #[test]
    fn cutting_a_memory_limit_carries_an_explicit_warning() {
        let usage = samples(50, 0.1, 25.0 * MIB);
        let recommendation = build("app", &usage, 7200, 0.1, 0.0, 128.0 * MIB, 512.0 * MIB);

        assert!(recommendation.recommended_memory_limit < 512.0 * MIB);
        let warning = recommendation
            .notes
            .iter()
            .find(|note| note.contains("cutting the memory limit"))
            .expect("a large reduction must be called out");
        assert!(warning.contains("killing the container"), "{warning}");
        assert!(warning.contains("512Mi"), "{warning}");
    }

    /// A modest change is not worth a warning; crying wolf costs the ones that
    /// matter.
    #[test]
    fn a_small_limit_change_is_not_warned_about() {
        let usage = samples(50, 0.1, 300.0 * MIB);
        let recommendation = build("app", &usage, 7200, 0.1, 0.0, 128.0 * MIB, 512.0 * MIB);
        assert!(
            !recommendation
                .notes
                .iter()
                .any(|note| note.contains("cutting the memory limit")),
            "{:?}",
            recommendation.notes
        );
    }

    #[test]
    fn missing_memory_request_is_called_out() {
        let recommendation = build(
            "app",
            &samples(50, 0.1, 50.0 * MIB),
            7200,
            0.1,
            0.0,
            0.0,
            0.0,
        );
        assert!(
            recommendation
                .notes
                .iter()
                .any(|note| note.contains("places this pod blind"))
        );
    }

    #[test]
    fn cpu_is_rounded_to_readable_millicores() {
        assert_eq!(round_cpu(0.137), 0.14);
        assert_eq!(round_cpu(1.21), 1.25);
        assert_eq!(round_cpu(0.0), 0.0);
    }

    #[test]
    fn memory_is_rounded_up_to_whole_mebibytes() {
        assert_eq!(round_memory(1.5 * MIB), 2.0 * MIB);
        assert_eq!(round_memory(0.0), 0.0);
    }
}
