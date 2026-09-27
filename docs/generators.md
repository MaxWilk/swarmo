# Random values in requests

Alongside `{{variable}}` interpolation, request templates support
`{{$generator(args)}}` tokens. They are evaluated **on every iteration**, so one
request stands in for a whole population of them rather than a load test
replaying the identical bytes thousands of times.

They work anywhere interpolation runs: the URL, query params, headers, the HTTP
body, and a gRPC request's address, metadata and message.

```json
{
  "userId":  "{{$uuid}}",
  "country": "{{$pick(BD,IN,US)}}",
  "spend":   {{$float(0,100,2)}},
  "visits":  {{$int(1,50)}},
  "isNew":   {{$bool}},
  "seenAt":  "{{$now}}"
}
```

## The generators

| Token | Result |
|---|---|
| `{{$int(1,50)}}` | A random integer, inclusive at both ends. Bounds may be given either way round. |
| `{{$float(0,1)}}` | A random float, 6 decimals by default. |
| `{{$float(0,1,3)}}` | The same with 3 decimals. |
| `{{$pick(a,b,c)}}` | One of the listed values. |
| `{{$string(12)}}` | A random 12-character alphanumeric string. Length defaults to 8. |
| `{{$string(12,hex)}}` | The same from a chosen character set: `alnum`, `alpha`, `lower`, `digits`, `hex`. |
| `{{$uuid}}` | A version 4 UUID. |
| `{{$bool}}` | `true` or `false`. |
| `{{$now}}` | The current time as RFC 3339 — also the proto3 JSON form of `Timestamp`. |
| `{{$now(3600)}}` | An hour from now. Negative offsets look backwards. |
| `{{$epoch}}` / `{{$epochMs}}` | Unix time in seconds or milliseconds. |

`randomInt`, `randomFloat`, `randomString`, `oneOf` and `choice` are accepted as
aliases, so snippets copied from other tools usually work unchanged.

## Transforms

Pipe the value through one or more transforms:

```text
{{$pick(vivo,samsung) | base64}}
{{$string(4) | upper | base64}}
```

Available: `base64` (also `b64`), `upper`, `lower`, `trim`.

`base64` is the one that matters for gRPC. A `DT_STRING` tensor's value is a
`bytes` field, which proto3 JSON encodes as base64, so a randomised string
tensor looks like this:

```json
"geo": {
  "dtype": "DT_STRING",
  "tensorShape": { "dim": [{ "size": "1" }, { "size": "1" }] },
  "stringVal": ["{{$pick(BD,IN,US) | base64}}"]
}
```

## Two rules worth remembering

**Quote strings, leave numbers bare.** The token is replaced with plain text, so
the surrounding JSON has to already be the right shape:

```json
{ "name": "{{$string(6)}}", "count": {{$int(1,9)}}, "ok": {{$bool}} }
```

Quoting a number produces `"count": "7"`, which most servers reject.

**Quote choices that contain commas, meaningful spaces, or nothing at all** —
and inside a JSON body, **use single quotes**. A double quote would close the
surrounding JSON string and leave you with a parse error:

```json
{ "carrier": "{{$pick('airtel','grameenphone','')}}" }
```

Both quote styles are accepted, and only the matching one closes, so
`{{$pick('say "hi"')}}` keeps its inner quotes. Unquoted arguments are trimmed,
so `{{$pick(a, b)}}` and `{{$pick(a,b)}}` mean the same thing.

## How they interact with variables

Variables resolve first, then generators. So a variable whose *value* is a
generator token still expands, and generator arguments see resolved text.

A token that cannot be understood — an unknown generator, a bad argument — is
**left in place** and reported as unresolved, rather than quietly becoming an
empty string. You will see it flagged in the request editor and, if you send
anyway, in whatever error the malformed body produces.

## Performance note

Swarmo parses a gRPC request message once at planning time and reuses it, which
matters when the message is a large tensor. A message containing generators
cannot be cached that way — it has to be rebuilt per iteration, which is the
whole point. If you are pushing very high rates with a large payload, randomise
a small field rather than the bulk of the message.
