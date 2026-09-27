import { useState } from "react";
import { Icons } from "./ui";

interface GeneratorToken {
  token: string;
  desc: string;
}

const TOKENS: GeneratorToken[] = [
  { token: "{{$int(1,50)}}", desc: "Random integer, inclusive." },
  { token: "{{$float(0,1,3)}}", desc: "Random float, 3 decimals (decimals optional, default 6)." },
  {
    token: '{{$pick(a,b,c)}}',
    desc: 'One of the listed values. Quote to include commas or spaces: {{$pick("a,b"," c ")}}.',
  },
  {
    token: "{{$string(12)}}",
    desc: "Random string. Optional charset second arg: alnum (default), alpha, lower, digits, hex, base36 (0-9a-z).",
  },
  { token: "{{$uuid}}", desc: "A UUID." },
  { token: "{{$bool}}", desc: "true or false." },
  { token: "{{$now}} / {{$now(3600)}}", desc: "RFC 3339 timestamp, optional offset in seconds." },
  { token: "{{$epoch}} / {{$epochMs}}", desc: "Unix time, in seconds or milliseconds." },
  {
    token: "{{$repeat(1000, $uuid, ',')}}",
    desc: "The inner value that many times, joined by the separator (default a comma). Up to 1,000,000.",
  },
  {
    token: "{{$repeat(1000, $seq | pad(6) | fmt('creative-{}'))}}",
    desc: "$seq is the position inside the repeat, so this makes creative-000000 … creative-000999 — guaranteed unique. pad(6) zero-pads to six digits.",
  },
  {
    token: "{{$repeatUnique(1000, $int(0,9999999) | fmt('creative-{}'))}}",
    desc: "Like repeat, but duplicates are redrawn, so random values still come out all distinct.",
  },
  {
    token: "[{{$repeat(1000, $uuid | quote)}}]",
    desc: "A JSON array of strings. Without quote the values arrive bare and the body will not parse.",
  },
  {
    token: "{{$repeat(512, $float(0,1,4)) | b64f32}}",
    desc: "An embedding as a base64 vector: the numbers packed as little-endian float32 bytes, then encoded. b64f64 for double precision.",
  },
  {
    token: "{{$repeat(1000, $seq) | b64u128}}",
    desc: "Integer keys packed as little-endian 16-byte values, then base64 — for a UInt128[] behind a base64 converter.",
  },
];

/** Collapsible reference for the {{$generator(args)}} tokens usable in bodies and messages. */
export function GeneratorHelp() {
  const [open, setOpen] = useState(false);

  return (
    <div className="col">
      <button
        className="btn ghost sm"
        style={{ alignSelf: "flex-start" }}
        onClick={() => setOpen((v) => !v)}
      >
        {open ? <Icons.ChevronDown /> : <Icons.Chevron />}
        Random values
      </button>

      {open && (
        <div className="col" style={{ gap: 6 }}>
          <table className="data-table">
            <thead>
              <tr>
                <th>Token</th>
                <th style={{ textAlign: "left" }}>What it does</th>
              </tr>
            </thead>
            <tbody>
              {TOKENS.map((t) => (
                <tr key={t.token}>
                  <td className="mono">{t.token}</td>
                  <td style={{ textAlign: "left", whiteSpace: "normal" }}>{t.desc}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="hint">
            Transforms pipe onto the value, e.g. <span className="mono">{'{{$pick(a,b) | base64}}'}</span>. Also{" "}
            <span className="mono">upper</span>, <span className="mono">lower</span>,{" "}
            <span className="mono">trim</span>, <span className="mono">{"fmt('id-{}')"}</span>,{" "}
            <span className="mono">pad(6)</span> and <span className="mono">quote</span>, and
            they can be chained:{" "}
            <span className="mono">{'{{$string(4) | upper | base64}}'}</span>.
          </div>
          <div className="hint">
            Two different things are called base64.{" "}
            <span className="mono">base64</span> encodes the value as text, so a list comes
            back as <span className="mono">"0.64,0.81"</span>.{" "}
            <span className="mono">b64f32</span> (or <span className="mono">b64f64</span>)
            packs the numbers as raw little-endian IEEE-754 bytes first — the format an
            embeddings endpoint means, with <span className="mono">b64u128</span> doing the
            same for 16-byte integer keys. Quote it, since it is a JSON string:{" "}
            <span className="mono">{'"embedding": "{{$repeat(512, $float(0,1,4)) | b64f32}}"'}</span>.
          </div>
          <div className="hint">
            In a JSON body, a list of <em>strings</em> needs each value quoted —{" "}
            <span className="mono">| quote</span> does that, and escapes the value too:{" "}
            <span className="mono">{"\"ids\": [{{$repeat(100, $uuid | quote)}}]"}</span>. Numbers
            need no quoting, so <span className="mono">{"[{{$repeat(512, $float(0,1,4))}}]"}</span>{" "}
            is already valid.
          </div>
          <div className="hint">
            In JSON bodies, numeric and boolean tokens must be unquoted (
            <span className="mono">{'"n": {{$int(1,9)}}'}</span>), while string ones stay quoted (
            <span className="mono">{'"s": "{{$uuid}}"'}</span>).
          </div>
        </div>
      )}
    </div>
  );
}
