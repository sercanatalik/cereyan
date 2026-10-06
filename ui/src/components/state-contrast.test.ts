// Every state badge's label must reach 4.5:1 against its tint, in both themes.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { SUB_STATE_STYLE, TYPE_STYLE } from "./state-badge";

const THEME = readFileSync(resolve(__dirname, "../../node_modules/tailwindcss/theme.css"), "utf8");
const APP = readFileSync(resolve(__dirname, "../index.css"), "utf8");

type Rgb = [number, number, number];

function parseOklch(text: string): [number, number, number] {
  const m = text.match(/oklch\(\s*([\d.]+)(%?)\s+([\d.]+)\s+([\d.]+)/);
  if (!m) throw new Error(`not an oklch colour: ${text}`);
  const l = Number(m[1]) / (m[2] ? 100 : 1);
  return [l, Number(m[3]), Number(m[4])];
}

/** Linear sRGB of an oklch colour, clamped to the gamut. */
function oklchToLinear([l, c, h]: [number, number, number]): Rgb {
  const a = c * Math.cos((h * Math.PI) / 180);
  const b = c * Math.sin((h * Math.PI) / 180);
  const l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s_ = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const clamp = (v: number) => Math.min(1, Math.max(0, v));
  return [
    clamp(4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_),
    clamp(-1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_),
    clamp(-0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_),
  ];
}

// Alpha blending happens in gamma-encoded sRGB, as browsers do.
const encode = (v: number) => (v <= 0.0031308 ? 12.92 * v : 1.055 * v ** (1 / 2.4) - 0.055);
const decode = (v: number) => (v <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);

function blend(top: Rgb, alpha: number, under: Rgb): Rgb {
  return top.map((t, i) => decode(alpha * encode(t) + (1 - alpha) * encode(under[i]))) as Rgb;
}

const luminance = ([r, g, b]: Rgb) => 0.2126 * r + 0.7152 * g + 0.0722 * b;
function ratio(x: Rgb, y: Rgb): number {
  const [hi, lo] = [luminance(x), luminance(y)].sort((p, q) => q - p);
  return (hi + 0.05) / (lo + 0.05);
}

function token(name: string): Rgb {
  const m = THEME.match(new RegExp(`--color-${name}:\\s*(oklch\\([^)]*\\))`));
  if (!m) throw new Error(`unknown colour ${name}`);
  return oklchToLinear(parseOklch(m[1]));
}

function appBackground(dark: boolean): Rgb {
  const block = dark ? APP.slice(APP.indexOf(".dark {")) : APP;
  const m = block.match(/--background:\s*(oklch\([^)]*\))/);
  if (!m) throw new Error("no --background");
  return oklchToLinear(parseOklch(m[1]));
}

/** The label and tint a pill's classes resolve to in one theme. */
function pillColours(pill: string, dark: boolean): { text: Rgb; tint: Rgb } {
  const pick = (kind: "bg" | "text") => {
    const re = new RegExp(`(?:^|\\s)${dark ? "dark:" : ""}${kind}-([a-z]+-\\d+)(?:/(\\d+))?(?=\\s|$)`);
    const m = pill.match(re);
    if (!m) throw new Error(`no ${dark ? "dark " : ""}${kind} in "${pill}"`);
    return { colour: token(m[1]), alpha: m[2] ? Number(m[2]) / 100 : 1 };
  };
  const bg = pick("bg");
  const text = pick("text");
  return { text: text.colour, tint: blend(bg.colour, bg.alpha, appBackground(dark)) };
}

const PILLS = [
  ...Object.entries(TYPE_STYLE).map(([name, s]) => [name, s.pill] as const),
  ...Object.entries(SUB_STATE_STYLE).map(([name, s]) => [name, s.pill] as const),
];

describe.each(PILLS)("%s badge", (_name, pill) => {
  test.each([
    ["light", false],
    ["dark", true],
  ])("label reaches 4.5:1 on its tint in the %s theme", (_theme, dark) => {
    const { text, tint } = pillColours(pill, dark);
    expect(ratio(text, tint)).toBeGreaterThanOrEqual(4.5);
  });
});
