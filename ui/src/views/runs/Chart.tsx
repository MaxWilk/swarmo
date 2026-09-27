import { useEffect, useRef, useState } from "react";
import uPlot from "uplot";

interface ChartProps {
  title: string;
  data: uPlot.AlignedData;
  series: uPlot.Series[];
  height?: number;
  /** Title for the x axis. */
  xLabel?: string;
  /**
   * Title for each y axis, keyed by the `scale` its series use. Series that
   * set no scale sit on uPlot's default, which is named `"y"`.
   */
  yLabels?: Record<string, string>;
}

/** Thin wrapper around uPlot that re-creates the chart on theme change and resizes with its host. */
export function Chart({
  title,
  data,
  series,
  height = 180,
  xLabel = "Elapsed (s)",
  yLabels = {},
}: ChartProps) {
  const host = useRef<HTMLDivElement | null>(null);
  const plot = useRef<uPlot | null>(null);
  const dataRef = useRef(data);
  const seriesRef = useRef(series);
  dataRef.current = data;
  seriesRef.current = series;

  useEffect(() => {
    if (!host.current) return;

    const build = () => {
      plot.current?.destroy();
      if (!host.current) return;
      const width = host.current.clientWidth || 300;

      // uPlot draws axis labels and grid lines onto a canvas, so they cannot
      // inherit colour from CSS. Left at their defaults they are near-black,
      // which is invisible on a dark background — hence reading the theme's
      // tokens here, and rebuilding the chart when the theme changes.
      const label = cssVar("--text-muted") || "#5c6470";
      const grid = cssVar("--border") || "#dfe3e8";
      const font = `11px ${cssVar("--font") || "sans-serif"}`;
      const base: uPlot.Axis = {
        stroke: label,
        font,
        labelFont: `600 11px ${cssVar("--font") || "sans-serif"}`,
        labelSize: 16,
        grid: { stroke: grid, width: 1 },
        ticks: { stroke: grid, width: 1 },
      };

      // One y axis per distinct scale, because uPlot only draws ticks for a
      // scale an axis is bound to. A single default axis leaves any series on
      // a named scale — the error-rate line, say — with a bare, numberless
      // edge. The first goes left, the rest right.
      const scales: string[] = [];
      for (const s of seriesRef.current.slice(1)) {
        const scale = s.scale ?? "y";
        if (!scales.includes(scale)) scales.push(scale);
      }
      if (scales.length === 0) scales.push("y");

      const axes: uPlot.Axis[] = [{ ...base, scale: "x", label: xLabel }];
      for (const [i, scale] of scales.entries()) {
        axes.push({
          ...base,
          scale,
          side: i === 0 ? 3 : 1,
          label: yLabels[scale],
          // Only the first axis draws the grid; a second set of lines on
          // different values would just be noise across the plot.
          grid: i === 0 ? base.grid : { show: false },
        });
      }

      plot.current = new uPlot(
        {
          width,
          height,
          series: seriesRef.current,
          axes,
          legend: { show: true },
          // x is elapsed seconds, not a timestamp. Left on uPlot's default the
          // ticks are formatted as clock times off the epoch — a 60 second run
          // reads as "12/31/69 7:00pm".
          scales: { x: { time: false } },
        },
        dataRef.current,
        host.current,
      );
    };

    build();

    const ro = new ResizeObserver(() => {
      if (plot.current && host.current) {
        plot.current.setSize({ width: host.current.clientWidth || 300, height });
      }
    });
    if (host.current) ro.observe(host.current);

    const mo = new MutationObserver((muts) => {
      if (muts.some((m) => m.attributeName === "data-theme")) build();
    });
    mo.observe(document.documentElement, { attributes: true });

    return () => {
      ro.disconnect();
      mo.disconnect();
      plot.current?.destroy();
      plot.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
    // Strokes are part of the deps: they are read from CSS variables, so a
    // theme change produces new ones, and only a rebuild applies them.
  }, [
    height,
    xLabel,
    JSON.stringify(yLabels),
    series.map((s) => s.scale ?? "y").join(),
    series.map((s) => s.stroke).join(),
  ]);

  useEffect(() => {
    plot.current?.setData(data);
  }, [data]);

  return (
    <div className="col" style={{ gap: 4 }}>
      <div className="hint">{title}</div>
      <div ref={host} style={{ width: "100%", height }} />
    </div>
  );
}

/** Reads a CSS custom property from the root element as a color string. */
export function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/** Bumps whenever `data-theme` changes on the root element, so callers can recompute colors. */
export function useThemeVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => {
    const mo = new MutationObserver((muts) => {
      if (muts.some((m) => m.attributeName === "data-theme")) setVersion((v) => v + 1);
    });
    mo.observe(document.documentElement, { attributes: true });
    return () => mo.disconnect();
  }, []);
  return version;
}
