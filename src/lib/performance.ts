export interface PerformanceSummary {
  samples: number;
  p50: number;
  p95: number;
  max: number;
}

export interface FrameSample {
  durationMs: number;
  frames: number;
  fps: number;
}

const inputLatencies: number[] = [];
const enabled = typeof window !== "undefined" && (
  import.meta.env.VITE_INKFLOW_PERF === "1"
  || window.localStorage.getItem("inkflow.performance") === "1"
);

export function inputStarted(): number {
  return enabled ? performance.now() : 0;
}

export function inputCommitted(startedAt: number): void {
  if (!enabled || startedAt <= 0) return;
  inputLatencies.push(performance.now() - startedAt);
  if (inputLatencies.length > 2_000) inputLatencies.splice(0, inputLatencies.length - 2_000);
}

export function markInteractive(): void {
  if (!enabled) return;
  performance.mark("inkflow-interactive");
  if (performance.getEntriesByName("inkflow-bootstrap").length) {
    performance.measure("inkflow-startup", "inkflow-bootstrap", "inkflow-interactive");
  }
}

export function summarizeInput(): PerformanceSummary {
  if (!inputLatencies.length) return { samples: 0, p50: 0, p95: 0, max: 0 };
  const sorted = [...inputLatencies].sort((left, right) => left - right);
  const percentile = (value: number) => sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * value))];
  return {
    samples: sorted.length,
    p50: percentile(.5),
    p95: percentile(.95),
    max: sorted.at(-1) ?? 0,
  };
}

export function sampleFrames(durationMs = 3_000): Promise<FrameSample> {
  return new Promise((resolve) => {
    const started = performance.now();
    let frames = 0;
    const frame = (now: number) => {
      frames += 1;
      const elapsed = now - started;
      if (elapsed >= durationMs) {
        resolve({ durationMs: elapsed, frames, fps: frames * 1_000 / elapsed });
      } else {
        requestAnimationFrame(frame);
      }
    };
    requestAnimationFrame(frame);
  });
}

if (enabled && typeof window !== "undefined") {
  performance.mark("inkflow-bootstrap");
  window.__INKFLOW_PERFORMANCE__ = {
    input: summarizeInput,
    sampleFrames,
    reset: () => inputLatencies.splice(0),
  };
}
