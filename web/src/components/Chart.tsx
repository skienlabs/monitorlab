import { useEffect, useMemo, useRef } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { useTheme } from "../theme";

export interface SeriesDef {
  label: string;
  values: (number | null)[];
  /** CSS custom property holding the color, e.g. "--series-1". */
  color: string;
}

interface Props {
  ts: number[]; // unix seconds
  series: SeriesDef[];
  fmt: (v: number) => string;
  height?: number;
  /** Fixed upper bound (e.g. 100 for percentages). */
  yMax?: number;
  /** Show time with seconds (live charts). */
  live?: boolean;
  empty?: string;
  /** Smallest y-axis ceiling, so near-zero data does not produce duplicate tick labels. */
  minTop?: number;
}

function cssVar(name: string) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || "#888";
}

function hexAlpha(hex: string, a: number) {
  const h = hex.replace("#", "");
  if (h.length !== 6) return hex;
  const n = parseInt(h, 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}

function tooltipPlugin(fmt: (v: number) => string, labels: string[], colors: string[], live: boolean): uPlot.Plugin {
  let tip: HTMLDivElement;
  return {
    hooks: {
      init: (u) => {
        tip = document.createElement("div");
        tip.className = "chart-tip";
        tip.style.display = "none";
        u.over.appendChild(tip);
        u.over.addEventListener("mouseleave", () => (tip.style.display = "none"));
        u.over.addEventListener("mouseenter", () => (tip.style.display = "block"));
      },
      setCursor: (u) => {
        const idx = u.cursor.idx;
        if (idx == null || u.cursor.left == null || u.cursor.left < 0) {
          tip.style.display = "none";
          return;
        }
        const t = u.data[0][idx];
        const d = new Date(t * 1000);
        const time = d.toLocaleString(undefined, live ? { timeStyle: "medium" } : { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
        let rows = "";
        for (let i = 1; i < u.data.length; i++) {
          const v = u.data[i][idx];
          rows += `<div class="r"><i style="background:${colors[i - 1]}"></i>${labels[i - 1]}<b>${v == null ? "—" : fmt(v as number)}</b></div>`;
        }
        tip.innerHTML = `<div class="t">${time}</div>${rows}`;
        tip.style.display = "block";
        const ow = u.over.clientWidth;
        const tw = tip.offsetWidth;
        const left = u.cursor.left + 14 + tw > ow ? u.cursor.left - tw - 14 : u.cursor.left + 14;
        tip.style.left = `${Math.max(0, left)}px`;
        tip.style.top = `${Math.max(0, (u.cursor.top ?? 0) - 20)}px`;
      },
    },
  };
}

export function TimeChart({ ts, series, fmt, height = 180, yMax, live = false, empty = "No data for this period", minTop = 0 }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const plot = useRef<uPlot | null>(null);
  const { resolved } = useTheme();
  const shape = series.map((s) => s.label + s.color).join("|");

  const data = useMemo(() => [ts, ...series.map((s) => s.values)] as uPlot.AlignedData, [ts, series]);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const colors = series.map((s) => cssVar(s.color));
    const grid = cssVar("--chart-grid");
    const axis = cssVar("--chart-axis");
    const font = `11.5px ${getComputedStyle(document.body).fontFamily}`;
    const single = series.length === 1;
    const opts: uPlot.Options = {
      width: el.clientWidth || 600,
      height,
      legend: { show: false },
      cursor: { points: { size: 8, width: 2 }, drag: { x: false, y: false }, y: false },
      padding: [8, 8, 0, 0],
      scales: {
        x: { time: true },
        y: {
          range: (_u, _min, max) => {
            if (yMax != null) return [0, Math.max(yMax, max)];
            const top = Math.max(max > 0 ? max * 1.15 : 1, minTop);
            return [0, top];
          },
        },
      },
      axes: [
        {
          stroke: axis,
          font,
          grid: { show: false },
          ticks: { show: false },
          gap: 6,
          size: 28,
          // Single-line labels sized to the visible span (uPlot's default stacks a date under the first tick).
          values: (u, splits) => {
            const span = (u.scales.x.max ?? 0) - (u.scales.x.min ?? 0);
            return splits.map((v) => {
              const d = new Date(v * 1000);
              if (span > 3 * 86400) return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
              if (span > 86400) return d.toLocaleString(undefined, { weekday: "short", hour: "2-digit", minute: "2-digit" });
              return d.toLocaleTimeString(undefined, live && span < 600 ? { hour: "2-digit", minute: "2-digit", second: "2-digit" } : { hour: "2-digit", minute: "2-digit" });
            });
          },
        },
        {
          stroke: axis,
          font,
          grid: { stroke: grid, width: 1 },
          ticks: { show: false },
          size: 64,
          gap: 6,
          values: (_u, splits) => splits.map((v) => fmt(v)),
        },
      ],
      series: [
        {},
        ...series.map((s, i) => ({
          label: s.label,
          stroke: colors[i],
          width: 2,
          points: { show: false },
          fill: single ? hexAlpha(colors[i], 0.1) : undefined,
          spanGaps: false,
        })),
      ],
      plugins: [tooltipPlugin(fmt, series.map((s) => s.label), colors, live)],
    };
    plot.current?.destroy();
    plot.current = new uPlot(opts, data, el);
    const ro = new ResizeObserver(() => {
      if (plot.current && el.clientWidth > 0) plot.current.setSize({ width: el.clientWidth, height });
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
      plot.current?.destroy();
      plot.current = null;
    };
    // recreate only when the shape/theme changes; data updates go through setData
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shape, resolved, height, yMax, live, minTop]);

  useEffect(() => {
    plot.current?.setData(data);
  }, [data]);

  const hasData = ts.length > 1;
  return (
    <div>
      {series.length > 1 && (
        <div className="legend">
          {series.map((s) => (
            <span key={s.label}>
              <i style={{ background: `var(${s.color})` }} />
              {s.label}
            </span>
          ))}
        </div>
      )}
      <div className="chart-box" style={{ height }}>
        <div ref={box} style={{ visibility: hasData ? "visible" : "hidden" }} />
        {!hasData && (
          <div className="chart-empty" style={{ position: "absolute", inset: 0 }}>
            {empty}
          </div>
        )}
      </div>
    </div>
  );
}
