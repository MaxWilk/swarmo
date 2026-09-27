import { useLayoutEffect, useRef, useState } from "react";
import type { DistBucket } from "../../api";
import { fmtMs, fmtNum } from "../../components/ui";

/**
 * The shape of a latency distribution, as bars.
 *
 * Percentiles say where the mass sits but not what shape it is: a run whose
 * requests are half fast and half slow has the same p50 as one where every
 * request is mediocre. Only the distribution tells them apart.
 *
 * Drawn as inline SVG in the same style as the load editor's stage preview,
 * rather than through uPlot, because bars need none of uPlot's machinery.
 */
export function Histogram({ buckets, p999 }: { buckets: DistBucket[]; p999: number }) {
  const host = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(480);
  const [hover, setHover] = useState<number | null>(null);

  // Measured in a layout effect so the first paint is already the right width.
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return;
    const measure = () => setWidth(el.clientWidth || 480);
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  if (buckets.length === 0) return null;

  const H = 170;
  const padL = 52;
  const padR = 10;
  const padT = 10;
  const padB = 30;
  const W = Math.max(240, width);
  const plotW = W - padL - padR;
  const plotH = H - padT - padB;

  const maxCount = Math.max(1, ...buckets.map((b) => b.count));
  const maxMs = Math.max(1e-9, buckets[buckets.length - 1]?.upperMs ?? 1);
  const barW = Math.max(1, plotW / buckets.length);

  return (
    <div className="col" style={{ gap: 4 }}>
      <div className="hint">Latency distribution</div>
      <div ref={host} style={{ width: "100%" }}>
        <svg viewBox={`0 0 ${W} ${H}`} width="100%" height={H} className="stage-chart">
          {[0, 0.5, 1].map((f) => {
            const y = H - padB - f * plotH;
            return (
              <g key={f}>
                <line className="grid" x1={padL} y1={y} x2={W - padR} y2={y} />
                <text className="tick" x={padL - 6} y={y + 3.5} textAnchor="end">
                  {fmtNum(maxCount * f)}
                </text>
              </g>
            );
          })}
          {buckets.map((b, i) =>
            b.count === 0 ? null : (
              <rect
                key={i}
                className={`hist-bar ${hover === i ? "hot" : ""}`}
                x={padL + i * barW}
                y={H - padB - (b.count / maxCount) * plotH}
                width={Math.max(0.5, barW - 1)}
                height={(b.count / maxCount) * plotH}
                onMouseEnter={() => setHover(i)}
                onMouseLeave={() => setHover(null)}
              >
                <title>
                  {fmtNum(b.count)} requests up to {fmtMs(b.upperMs)}
                </title>
              </rect>
            ),
          )}
          {[0, 0.5, 1].map((f) => (
            <text
              key={f}
              className="tick"
              x={padL + f * plotW}
              y={H - padB + 14}
              textAnchor={f === 0 ? "start" : f === 1 ? "end" : "middle"}
            >
              {fmtMs(maxMs * f)}
            </text>
          ))}
          <text className="axis" x={padL + plotW / 2} y={H - 2} textAnchor="middle">
            Latency (ms)
          </text>
          <text
            className="axis"
            transform={`translate(11,${padT + plotH / 2}) rotate(-90)`}
            textAnchor="middle"
          >
            Requests
          </text>
        </svg>
      </div>
      <div className="hint">
        How the requests were actually spread, which percentiles alone cannot show. The
        slowest 0.1% reached {fmtMs(p999)}.
      </div>
    </div>
  );
}
