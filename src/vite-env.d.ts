/// <reference types="vite/client" />

interface Window {
  __INKFLOW_PERFORMANCE__?: {
    input: () => import("./lib/performance").PerformanceSummary;
    sampleFrames: (durationMs?: number) => Promise<import("./lib/performance").FrameSample>;
    reset: () => void;
  };
}
