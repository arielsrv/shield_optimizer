export type SurroundMode = "auto" | "never" | "always" | "manual" | "unset" | "unknown";
export type VerdictLevel = "good" | "warn" | "info";

export interface VideoFormat {
  label: string;
  mime: string;
  advertised: boolean;
  software: boolean;
  acceleration_unknown: boolean;
}

export interface DisplayModeEntry {
  width: number;
  height: number;
  fps: number;
  active: boolean;
}

export interface AudioPassthrough {
  mode: SurroundMode;
  enabled_formats: string[];
  raw_formats: string | null;
}

export interface Verdict {
  level: VerdictLevel;
  title: string;
  detail: string;
}

export interface MediaCapabilities {
  video: VideoFormat[];
  hdr_types: string[];
  modes: DisplayModeEntry[];
  audio: AudioPassthrough;
  match_content_frame_rate: string | null;
  verdicts: Verdict[];
}

export function formatSupport(v: VideoFormat): { label: string; cls: string } {
  if (!v.advertised) return { label: "Not advertised in available configuration", cls: "" };
  if (v.acceleration_unknown) return { label: v.software ? "Listed; software + unknown acceleration" : "Listed; acceleration unknown", cls: "" };
  return { label: "Software decoder listed", cls: "" };
}

export const matchContentLabel: Record<string, string> = {
  "0": "Never",
  "1": "Seamless only",
  "2": "Always",
};

export const surroundLabel: Record<SurroundMode, string> = {
  auto: "Auto",
  never: "Never",
  always: "Always",
  manual: "Manual",
  unset: "Default/unset",
  unknown: "Unknown setting",
};
