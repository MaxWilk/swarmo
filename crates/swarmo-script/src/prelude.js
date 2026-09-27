/* Swarmo script prelude.
 *
 * Implements the entire user-facing scripting API on top of a handful of
 * native primitives (__swarmo_log, __swarmo_http, __swarmo_sleep, ...).
 * Evaluated once per JS context before any user code runs.
 */
(function (global) {
  'use strict';

  // ---------------------------------------------------------------------
  // Internals
  // ---------------------------------------------------------------------

  var __swarmo = {
    __done: false,
    __err: null,
    vars: {},        // variables the script set
    unset: {},       // names unset by the script (sent back as '')
    baseVars: {},    // variables supplied by the host scope
    request: null,   // mutable outgoing request (pre-request only)
    response: null,  // frozen response (post-response only)
    tests: [],
    checks: {},      // name -> { passes, fails }  (VU scripts)
    groupStack: []
  };

  __swarmo.errText = function (e) {
    if (e === null || e === undefined) return 'null';
    if (e instanceof Error) {
      // QuickJS's `stack` holds only the frames, not the message line, so
      // build the familiar "Error: message\n  at ..." shape ourselves.
      var head = (e.name || 'Error') + ': ' + (e.message === undefined ? '' : e.message);
      var frames = e.stack ? String(e.stack) : '';
      return frames ? head + '\n' + frames : head;
    }
    if (typeof e === 'object') {
      try { return JSON.stringify(e); } catch (_) { return String(e); }
    }
    return String(e);
  };

  __swarmo.reset = function () {
    __swarmo.vars = {};
    __swarmo.unset = {};
    __swarmo.tests = [];
    __swarmo.checks = {};
    __swarmo.groupStack = [];
    __swarmo.__done = false;
    __swarmo.__err = null;
  };

  __swarmo.setScope = function (baseVars) {
    __swarmo.baseVars = baseVars || {};
    __swarmo.vars = {};
    __swarmo.unset = {};
  };

  __swarmo.setRequest = function (req) { __swarmo.request = req; };
  __swarmo.setResponse = function (res) { __swarmo.response = res ? makeResponse(res) : null; };

  /** Drain the check counters (the load engine reads these per iteration). */
  __swarmo.takeChecks = function () {
    var c = __swarmo.checks;
    __swarmo.checks = {};
    return c;
  };

  __swarmo.result = function () {
    return {
      vars: __swarmo.vars,
      request: __swarmo.request ? plainRequest(__swarmo.request) : __swarmo.request,
      tests: __swarmo.tests,
      checks: __swarmo.checks
    };
  };

  /** Coerce a script-mutated request back into the all-strings shape Rust
   *  reads, so `headers.push(['X-Ts', Date.now()])` cannot sink the result. */
  function plainRequest(r) {
    function str(v) { return v === undefined || v === null ? '' : String(v); }
    return {
      name: str(r.name),
      method: str(r.method),
      url: str(r.url),
      headers: headersToPairs(r.headers),
      body: typeof r.body === 'string' || r.body === undefined || r.body === null
        ? str(r.body)
        : JSON.stringify(r.body)
    };
  }

  function has(o, k) { return Object.prototype.hasOwnProperty.call(o, k); }

  function lookup(key) {
    if (has(__swarmo.unset, key)) return undefined;
    if (has(__swarmo.vars, key)) return __swarmo.vars[key];
    if (has(__swarmo.baseVars, key)) return __swarmo.baseVars[key];
    return undefined;
  }

  function setVar(key, value) {
    delete __swarmo.unset[String(key)];
    __swarmo.vars[String(key)] = value === undefined || value === null ? '' : String(value);
  }

  // The host's variable map holds strings only, so an unset variable goes
  // back as ''; within this run it reads as absent, as in Postman.
  function unsetVar(key) {
    setVar(key, '');
    __swarmo.unset[String(key)] = true;
  }

  function fmt(v) {
    if (typeof v === 'string') return v;
    if (v instanceof Error) return __swarmo.errText(v);
    if (v === undefined) return 'undefined';
    if (v === null) return 'null';
    try { return JSON.stringify(v); } catch (_) { return String(v); }
  }

  function joinArgs(args) {
    var out = [];
    for (var i = 0; i < args.length; i++) out.push(fmt(args[i]));
    return out.join(' ');
  }

  // ---------------------------------------------------------------------
  // Standard-ish globals QuickJS does not provide
  // ---------------------------------------------------------------------

  global.console = {
    log: function () { __swarmo_log('log', joinArgs(arguments)); },
    info: function () { __swarmo_log('info', joinArgs(arguments)); },
    warn: function () { __swarmo_log('warn', joinArgs(arguments)); },
    error: function () { __swarmo_log('error', joinArgs(arguments)); },
    debug: function () { __swarmo_log('log', joinArgs(arguments)); }
  };

  function invalidCharacter(msg) {
    var e = new Error(msg);
    e.name = 'InvalidCharacterError';
    return e;
  }

  // Browser semantics: "binary strings" of one char per byte, and a throw
  // (not an empty string) on bad input.
  global.btoa = function (s) {
    var out = __swarmo_b64encode(String(s));
    if (out === undefined) throw invalidCharacter('btoa: the string contains characters outside the Latin1 range');
    return out;
  };
  global.atob = function (s) {
    var out = __swarmo_b64decode(String(s));
    if (out === undefined) throw invalidCharacter('atob: the string is not valid base64');
    return out;
  };

  if (!global.crypto) global.crypto = {};
  global.crypto.randomUUID = function () { return __swarmo_uuid(); };

  // ---------------------------------------------------------------------
  // HTTP
  // ---------------------------------------------------------------------

  function headersToPairs(h) {
    var pairs = [];
    if (!h) return pairs;
    if (Array.isArray(h)) {
      for (var i = 0; i < h.length; i++) pairs.push([String(h[i][0]), String(h[i][1])]);
      return pairs;
    }
    for (var k in h) {
      if (Object.prototype.hasOwnProperty.call(h, k)) pairs.push([k, String(h[k])]);
    }
    return pairs;
  }

  function pairsToObject(pairs) {
    var o = {};
    if (!pairs) return o;
    for (var i = 0; i < pairs.length; i++) o[String(pairs[i][0]).toLowerCase()] = pairs[i][1];
    return o;
  }

  function appendQuery(url, params) {
    if (!params) return url;
    var parts = [];
    for (var k in params) {
      if (Object.prototype.hasOwnProperty.call(params, k)) {
        parts.push(encodeURIComponent(k) + '=' + encodeURIComponent(params[k]));
      }
    }
    if (!parts.length) return url;
    return url + (url.indexOf('?') >= 0 ? '&' : '?') + parts.join('&');
  }

  function makeResponse(raw) {
    var res = {
      status: raw.status,
      statusText: raw.statusText || '',
      headers: pairsToObject(raw.headers),
      headerList: raw.headers || [],
      body: raw.body === undefined || raw.body === null ? '' : String(raw.body),
      durationMs: raw.durationMs || 0
    };
    res.text = function () { return res.body; };
    res.json = function () {
      try {
        return JSON.parse(res.body);
      } catch (e) {
        throw new Error('response body is not valid JSON: ' + e.message);
      }
    };
    res.ok = res.status >= 200 && res.status < 300;
    return res;
  }

  /** The one place a request actually leaves the sandbox. */
  function doRequest(method, url, opts) {
    opts = opts || {};
    var body = null;
    var headers = headersToPairs(opts.headers);

    var hasCt = false;
    for (var i = 0; i < headers.length; i++) {
      if (String(headers[i][0]).toLowerCase() === 'content-type') { hasCt = true; break; }
    }

    if (opts.json !== undefined && opts.json !== null) {
      body = JSON.stringify(opts.json);
      if (!hasCt) headers.push(['Content-Type', 'application/json']);
    } else if (opts.body !== undefined && opts.body !== null) {
      body = typeof opts.body === 'string' ? opts.body : JSON.stringify(opts.body);
    } else if (opts.form) {
      // An object, or [key, value] pairs when a key repeats.
      var fields = headersToPairs(opts.form);
      var parts = [];
      for (var f = 0; f < fields.length; f++) {
        parts.push(encodeURIComponent(fields[f][0]) + '=' + encodeURIComponent(fields[f][1]));
      }
      body = parts.join('&');
      if (!hasCt) headers.push(['Content-Type', 'application/x-www-form-urlencoded']);
    }

    var finalUrl = appendQuery(String(url), opts.params);

    var payload = {
      method: String(method).toUpperCase(),
      url: finalUrl,
      headers: headers,
      body: body,
      tag: opts.tag || null
    };

    var raw = __swarmo_http(JSON.stringify(payload));
    var parsed = JSON.parse(raw);
    if (!parsed.ok) throw new Error(parsed.error || 'request failed');
    return makeResponse(parsed.res);
  }

  // ---------------------------------------------------------------------
  // Assertions
  // ---------------------------------------------------------------------

  /** Structural equality: key order does not matter, and (as with the
   *  JSON comparison this replaced) keys holding `undefined` are ignored. */
  function deepEqual(a, b, depth) {
    if (a === b) return true;
    if (a !== a && b !== b) return true; // NaN
    if (!a || !b || typeof a !== 'object' || typeof b !== 'object') return false;
    depth = depth || 0;
    if (depth > 200) return false; // a cycle, or absurdly deep
    if (Array.isArray(a) !== Array.isArray(b)) return false;
    if (a instanceof Date || b instanceof Date) {
      return a instanceof Date && b instanceof Date && a.getTime() === b.getTime();
    }
    if (a instanceof RegExp || b instanceof RegExp) return String(a) === String(b);
    if (Array.isArray(a)) {
      if (a.length !== b.length) return false;
      for (var i = 0; i < a.length; i++) if (!deepEqual(a[i], b[i], depth + 1)) return false;
      return true;
    }
    function defined(o) {
      return Object.keys(o).filter(function (k) { return o[k] !== undefined; });
    }
    var ka = defined(a), kb = defined(b);
    if (ka.length !== kb.length) return false;
    for (var j = 0; j < ka.length; j++) {
      if (!has(b, ka[j]) || !deepEqual(a[ka[j]], b[ka[j]], depth + 1)) return false;
    }
    return true;
  }

  function fail(msg) { throw new Error(msg); }

  function expect(actual) {
    var api = {
      toBe: function (e) {
        if (actual !== e) fail('expected ' + fmt(actual) + ' to be ' + fmt(e));
        return api;
      },
      toEqual: function (e) {
        if (!deepEqual(actual, e)) fail('expected ' + fmt(actual) + ' to equal ' + fmt(e));
        return api;
      },
      toContain: function (e) {
        var ok = false;
        if (typeof actual === 'string') ok = actual.indexOf(String(e)) >= 0;
        else if (Array.isArray(actual)) {
          for (var i = 0; i < actual.length; i++) { if (deepEqual(actual[i], e)) { ok = true; break; } }
        } else if (actual && typeof actual === 'object') {
          ok = Object.prototype.hasOwnProperty.call(actual, e);
        }
        if (!ok) fail('expected ' + fmt(actual) + ' to contain ' + fmt(e));
        return api;
      },
      toBeLessThan: function (e) {
        if (!(actual < e)) fail('expected ' + fmt(actual) + ' to be less than ' + fmt(e));
        return api;
      },
      toBeGreaterThan: function (e) {
        if (!(actual > e)) fail('expected ' + fmt(actual) + ' to be greater than ' + fmt(e));
        return api;
      },
      toMatch: function (re) {
        var r = re instanceof RegExp ? re : new RegExp(String(re));
        if (!r.test(String(actual))) fail('expected ' + fmt(actual) + ' to match ' + String(r));
        return api;
      },
      toBeTruthy: function () {
        if (!actual) fail('expected ' + fmt(actual) + ' to be truthy');
        return api;
      },
      toBeFalsy: function () {
        if (actual) fail('expected ' + fmt(actual) + ' to be falsy');
        return api;
      },
      toBeDefined: function () {
        if (actual === undefined) fail('expected value to be defined');
        return api;
      }
    };
    return api;
  }

  function typeOf(v) {
    if (v === null) return 'null';
    if (Array.isArray(v)) return 'array';
    if (v instanceof Date) return 'date';
    if (v instanceof RegExp) return 'regexp';
    if (v instanceof Error) return 'error';
    return typeof v;
  }

  function isEmpty(v) {
    if (v === null || v === undefined) return true;
    if (typeof v === 'string' || Array.isArray(v)) return v.length === 0;
    if (typeof v === 'object') return Object.keys(v).length === 0;
    return false;
  }

  /** A response's numeric status: `code` on pm.response, `status` elsewhere. */
  function statusOf(r) {
    if (!r || typeof r !== 'object') return undefined;
    if (typeof r.code === 'number') return r.code;
    if (typeof r.status === 'number') return r.status;
    return undefined;
  }

  function statusTextOf(r) {
    if (typeof r.statusText === 'string') return r.statusText;
    return typeof r.status === 'string' ? r.status : undefined;
  }

  function headerOf(r, name) {
    var h = r && r.headers;
    if (!h) return undefined;
    if (typeof h.get === 'function') return h.get(name);
    var want = String(name).toLowerCase();
    for (var k in h) if (has(h, k) && k.toLowerCase() === want) return h[k];
    return undefined;
  }

  /** Every element of `sub` in `all`, each one matched at most once. */
  function containsAll(all, sub) {
    var used = [];
    for (var i = 0; i < sub.length; i++) {
      var found = false;
      for (var j = 0; j < all.length; j++) {
        if (!used[j] && deepEqual(all[j], sub[i])) { used[j] = true; found = true; break; }
      }
      if (!found) return false;
    }
    return true;
  }

  /**
   * A chai-style assertion, so imported Postman scripts keep working.
   *
   * It follows chai's own rules rather than approximating them: the chain
   * words (`to`, `be`, `have` …) return the assertion itself, `not` and `deep`
   * set flags for whatever comes next, and the property forms — `.ok`,
   * `.true`, `.null`, `.exist`, `.empty` — assert when they are read, with no
   * call. That last point is the one that matters: `expect(x).to.be.true;` is
   * how everyone writes it, and a shim that made it a method would pass every
   * such test without checking anything.
   *
   * On a response (`pm.response`, or anything with a numeric status and a
   * `json()` method) Postman's extra assertions apply: `.status(n)`,
   * `.header(name)`, `.ok` meaning status 200, `.success` meaning 2xx, and so on.
   */
  function chai(actual, isResponse) {
    if (!isResponse) {
      isResponse = !!actual && typeof actual === 'object' &&
        typeof actual.json === 'function' && statusOf(actual) !== undefined;
    }
    var flags = { negate: false, deep: false, contains: false };
    // The jest-style matchers stay available, as they always have been.
    var a = expect(actual);

    function assert(pass, what) {
      if (flags.negate ? pass : !pass) {
        fail('expected ' + fmt(actual) + (flags.negate ? ' not' : '') + ' to ' + what);
      }
      return a;
    }
    function define(name, get) {
      Object.defineProperty(a, name, { get: get, configurable: true });
    }
    function chainWord(name) { define(name, function () { return a; }); }
    function flag(name, set) { define(name, function () { set(); return a; }); }
    function property(names, check) {
      names.forEach(function (n) { define(n, function () { check(); return a; }); });
    }
    function method(names, fn) { names.forEach(function (n) { a[n] = fn; }); }
    // Both a word in a chain and a call: `to.include(x)` and
    // `to.include.members([…])`, `to.be.a('string')` and `to.have.a.property(…)`.
    function chainable(names, onAccess, fn) {
      names.forEach(function (n) {
        define(n, function () {
          if (onAccess) onAccess();
          var f = function () { return fn.apply(null, arguments); };
          Object.setPrototypeOf(f, a);
          return f;
        });
      });
    }
    function needResponse(what) {
      if (statusOf(actual) === undefined) fail('expected a response to ' + what);
    }
    function statusIn(lo, hi, what) {
      return function () {
        needResponse('be ' + what);
        var st = statusOf(actual);
        var inside = st >= lo && st <= hi;
        if (flags.negate ? inside : !inside) {
          fail('expected response status ' + st + (flags.negate ? ' not' : '') + ' to be ' + what);
        }
      };
    }

    ['to', 'be', 'been', 'is', 'that', 'which', 'and', 'has', 'have', 'with',
     'at', 'of', 'same', 'but', 'does', 'still', 'also', 'all', 'any']
      .forEach(chainWord);
    flag('not', function () { flags.negate = !flags.negate; });
    flag('deep', function () { flags.deep = true; });

    property(['ok'], function () {
      if (isResponse) statusIn(200, 200, 'ok (200)')();
      else assert(!!actual, 'be ok');
    });
    property(['true'], function () { assert(actual === true, 'be true'); });
    property(['false'], function () { assert(actual === false, 'be false'); });
    property(['null'], function () { assert(actual === null, 'be null'); });
    property(['undefined'], function () { assert(actual === undefined, 'be undefined'); });
    property(['NaN'], function () { assert(actual !== actual, 'be NaN'); });
    property(['exist', 'exists'], function () {
      assert(actual !== null && actual !== undefined, 'exist');
    });
    property(['empty'], function () { assert(isEmpty(actual), 'be empty'); });

    // Postman's response assertions.
    property(['success'], statusIn(200, 299, 'a success (2xx)'));
    property(['info'], statusIn(100, 199, 'informational (1xx)'));
    property(['redirection'], statusIn(300, 399, 'a redirection (3xx)'));
    property(['clientError'], statusIn(400, 499, 'a client error (4xx)'));
    property(['serverError'], statusIn(500, 599, 'a server error (5xx)'));
    property(['error'], statusIn(400, 599, 'an error (4xx or 5xx)'));
    property(['accepted'], statusIn(202, 202, 'accepted (202)'));
    property(['badRequest'], statusIn(400, 400, 'a bad request (400)'));
    property(['unauthorized'], statusIn(401, 401, 'unauthorized (401)'));
    property(['forbidden'], statusIn(403, 403, 'forbidden (403)'));
    property(['notFound'], statusIn(404, 404, 'not found (404)'));
    property(['rateLimited'], statusIn(429, 429, 'rate limited (429)'));
    property(['json'], function () {
      needResponse('be JSON');
      var parsed = true;
      try { actual.json(); } catch (_) { parsed = false; }
      assert(parsed, 'have a JSON body');
    });

    method(['equal', 'equals', 'eq'], function (v) {
      return assert(flags.deep ? deepEqual(actual, v) : actual === v, 'equal ' + fmt(v));
    });
    method(['eql', 'eqls'], function (v) {
      return assert(deepEqual(actual, v), 'deeply equal ' + fmt(v));
    });

    chainable(['a', 'an'], null, function (t) {
      t = String(t).toLowerCase();
      return assert(typeOf(actual) === t, 'be a ' + t);
    });

    chainable(['include', 'includes', 'contain', 'contains'],
      function () { flags.contains = true; },
      function (v) {
        var ok;
        if (typeof actual === 'string') ok = actual.indexOf(String(v)) >= 0;
        else if (Array.isArray(actual)) ok = actual.some(function (x) { return deepEqual(x, v); });
        else if (actual && typeof actual === 'object' && v && typeof v === 'object') {
          ok = Object.keys(v).every(function (k) { return has(actual, k) && deepEqual(actual[k], v[k]); });
        } else if (actual && typeof actual === 'object') ok = has(actual, v);
        else ok = false;
        return assert(ok, 'include ' + fmt(v));
      });

    method(['match', 'matches'], function (re) {
      var r = re instanceof RegExp ? re : new RegExp(String(re));
      return assert(r.test(String(actual)), 'match ' + String(r));
    });
    method(['string'], function (sub) {
      return assert(typeof actual === 'string' && actual.indexOf(sub) >= 0, 'contain ' + fmt(sub));
    });

    // Without a value, `property` moves the subject on to the property, so
    // `.to.have.property('id').that.is.a('number')` checks the id.
    method(['property'], function (name, value) {
      var present = actual !== null && actual !== undefined && Object(actual)[name] !== undefined;
      if (arguments.length > 1) {
        return assert(present && deepEqual(actual[name], value),
          'have property ' + fmt(name) + ' of ' + fmt(value));
      }
      assert(present, 'have property ' + fmt(name));
      return flags.negate ? a : chai(actual[name]);
    });
    method(['lengthOf', 'length'], function (n) {
      var len;
      if (actual !== null && actual !== undefined && actual.length !== undefined) len = actual.length;
      else if (actual && typeof actual === 'object') len = Object.keys(actual).length;
      return assert(len === n, 'have length ' + n + ' (got ' + fmt(len) + ')');
    });
    method(['members'], function (list) {
      var ok = Array.isArray(actual) && Array.isArray(list) &&
        (flags.contains
          ? containsAll(actual, list)
          : actual.length === list.length && containsAll(actual, list));
      return assert(ok, (flags.contains ? 'include members ' : 'have the same members as ') + fmt(list));
    });
    method(['keys', 'key'], function () {
      var first = arguments[0];
      var want = arguments.length === 1 && first && typeof first === 'object'
        ? (Array.isArray(first) ? first : Object.keys(first))
        : Array.prototype.slice.call(arguments);
      var got = actual && typeof actual === 'object' ? Object.keys(actual) : [];
      var ok = want.every(function (k) { return got.indexOf(String(k)) >= 0; }) &&
        (flags.contains || got.length === want.length);
      return assert(ok, 'have keys ' + fmt(want));
    });

    method(['above', 'gt', 'greaterThan'], function (n) { return assert(actual > n, 'be above ' + n); });
    method(['below', 'lt', 'lessThan'], function (n) { return assert(actual < n, 'be below ' + n); });
    method(['least', 'gte'], function (n) { return assert(actual >= n, 'be at least ' + n); });
    method(['most', 'lte'], function (n) { return assert(actual <= n, 'be at most ' + n); });
    method(['within'], function (lo, hi) {
      return assert(actual >= lo && actual <= hi, 'be within ' + lo + '..' + hi);
    });
    method(['closeTo', 'approximately'], function (n, delta) {
      return assert(Math.abs(actual - n) <= delta, 'be within ' + delta + ' of ' + n);
    });
    method(['oneOf'], function (list) {
      return assert(list.some(function (x) { return deepEqual(x, actual); }), 'be one of ' + fmt(list));
    });
    method(['instanceof', 'instanceOf'], function (ctor) {
      return assert(actual instanceof ctor, 'be an instance of ' + (ctor && ctor.name));
    });
    method(['satisfy', 'satisfies'], function (fn) {
      return assert(!!fn(actual), 'satisfy the given function');
    });
    method(['throw', 'throws', 'Throw'], function (want) {
      var err = null;
      try { actual(); } catch (e) { err = e; }
      var ok = err !== null;
      if (ok && want !== undefined) {
        var msg = err && err.message !== undefined ? String(err.message) : String(err);
        if (want instanceof RegExp) ok = want.test(msg);
        else if (typeof want === 'string') ok = msg.indexOf(want) >= 0;
        else if (typeof want === 'function') ok = err instanceof want;
      }
      return assert(ok, 'throw' + (want !== undefined ? ' ' + String(want) : ''));
    });

    method(['status'], function (want) {
      needResponse('have a status');
      var got = typeof want === 'string' ? statusTextOf(actual) : statusOf(actual);
      return assert(got === want, 'have status ' + fmt(want) + ' (got ' + fmt(got) + ')');
    });
    method(['header'], function (name, value) {
      var got = headerOf(actual, name);
      if (arguments.length > 1) {
        return assert(got === value,
          'have header ' + fmt(name) + ' of ' + fmt(value) + ' (got ' + fmt(got) + ')');
      }
      return assert(got !== undefined, 'have header ' + fmt(name));
    });
    method(['body'], function (want) {
      needResponse('have a body');
      var text = actual.text();
      if (arguments.length === 0) return assert(text !== '', 'have a body');
      return assert(want instanceof RegExp ? want.test(text) : text === want, 'have body ' + fmt(want));
    });
    method(['jsonBody'], function (path, value) {
      needResponse('have a JSON body');
      var v;
      try { v = actual.json(); } catch (_) { return assert(false, 'have a JSON body'); }
      if (arguments.length === 0) return assert(true, 'have a JSON body');
      String(path).split('.').forEach(function (k) {
        v = v !== null && v !== undefined ? v[k] : undefined;
      });
      if (arguments.length === 1) return assert(v !== undefined, 'have JSON at ' + fmt(path));
      return assert(deepEqual(v, value), 'have JSON ' + fmt(value) + ' at ' + fmt(path));
    });

    return a;
  }

  function recordTest(name, fn) {
    var entry = { name: String(name), passed: true };
    function failed(err) {
      entry.passed = false;
      entry.error = err && err.message ? String(err.message) : __swarmo.errText(err);
    }
    var ret;
    try {
      ret = fn();
    } catch (err) {
      failed(err);
    }
    // An async test fails when its promise rejects. The job queue is
    // drained before results are read, so this lands in time.
    if (ret && typeof ret.then === 'function') ret.then(null, failed);
    __swarmo.tests.push(entry);
    return entry.passed;
  }

  // ---------------------------------------------------------------------
  // sw  (the native Swarmo API)
  // ---------------------------------------------------------------------

  var sw = {
    env: {
      get: function (k) { return lookup(k); },
      set: function (k, v) { setVar(k, v); },
      has: function (k) { return lookup(k) !== undefined; },
      unset: function (k) { unsetVar(k); },
      toObject: function () {
        var o = {};
        for (var k in __swarmo.baseVars) o[k] = __swarmo.baseVars[k];
        for (var k2 in __swarmo.vars) o[k2] = __swarmo.vars[k2];
        for (var k3 in __swarmo.unset) delete o[k3];
        return o;
      }
    },
    test: recordTest,
    expect: expect,
    sleep: function (ms) { __swarmo_sleep(Number(ms) || 0); },
    interpolate: function (s) { return __swarmo_interp(String(s)); },
    sendRequest: function (opts) {
      if (typeof opts === 'string') return doRequest('GET', opts, {});
      opts = opts || {};
      return doRequest(opts.method || 'GET', opts.url, opts);
    },
    get request() { return __swarmo.request; },
    get response() { return __swarmo.response; }
  };
  sw.vars = sw.env;

  // ---------------------------------------------------------------------
  // pm  (Postman compatibility shim)
  // ---------------------------------------------------------------------

  function unsupported(name) {
    return function () {
      throw new Error('Swarmo: ' + name + ' is not supported');
    };
  }

  function unsupportedObject(name, members) {
    var o = {};
    for (var i = 0; i < members.length; i++) o[members[i]] = unsupported(name + '.' + members[i]);
    return o;
  }

  var envShim = {
    get: function (k) { return lookup(k); },
    set: function (k, v) { setVar(k, v); },
    has: function (k) { return lookup(k) !== undefined; },
    unset: function (k) { unsetVar(k); },
    toObject: function () { return sw.env.toObject(); },
    clear: unsupported('pm.environment.clear')
  };

  /** Postman's key/value lists: `[{key, value, disabled}]`, `[[k, v]]`, or an
   *  object. Returned as [name, value] pairs (objects pass through). */
  function postmanPairs(list) {
    if (!Array.isArray(list)) return list;
    var out = [];
    for (var i = 0; i < list.length; i++) {
      var item = list[i];
      if (Array.isArray(item)) out.push(item);
      else if (item && !item.disabled) out.push([item.key, item.value === undefined || item.value === null ? '' : item.value]);
    }
    return out;
  }

  /** Map Postman's request shape (`header`, `url: {raw}`, `body: {mode, ...}`)
   *  onto sw.sendRequest's. Swarmo-shaped options pass through untouched. */
  function fromPostmanRequest(opts) {
    if (!opts || typeof opts !== 'object') return opts;
    var o = {};
    for (var k in opts) if (has(opts, k)) o[k] = opts[k];

    if (o.url && typeof o.url === 'object') o.url = o.url.raw !== undefined ? o.url.raw : String(o.url);
    if (o.headers === undefined && o.header !== undefined) o.headers = headersToPairs(postmanPairs(o.header));

    var b = o.body;
    if (b && typeof b === 'object' && typeof b.mode === 'string') {
      delete o.body;
      if (b.mode === 'raw') {
        o.body = b.raw === undefined || b.raw === null ? '' : String(b.raw);
        var lang = b.options && b.options.raw && b.options.raw.language;
        var headers = headersToPairs(o.headers);
        var hasCt = headers.some(function (h) { return String(h[0]).toLowerCase() === 'content-type'; });
        if (lang === 'json' && !hasCt) headers.push(['Content-Type', 'application/json']);
        o.headers = headers;
      } else if (b.mode === 'urlencoded') {
        o.form = postmanPairs(b.urlencoded || []);
      } else if (b.mode === 'graphql') {
        var g = b.graphql || {};
        var vars = g.variables;
        if (typeof vars === 'string') vars = vars.trim() ? JSON.parse(vars) : {};
        o.json = { query: g.query || '', variables: vars || {} };
      } else {
        throw new Error('Swarmo: pm.sendRequest body mode "' + b.mode + '" is not supported');
      }
    }
    return o;
  }

  /** Swarmo's response, plus the Postman names callbacks reach for (`code`,
   *  `responseTime`, `headers.get`). `status` stays the number, as in sw. */
  function postmanResponse(res) {
    res.code = res.status;
    res.responseTime = res.durationMs;
    var h = res.headers;
    Object.defineProperty(h, 'get', { value: function (name) { return h[String(name).toLowerCase()]; } });
    Object.defineProperty(h, 'has', { value: function (name) { return h[String(name).toLowerCase()] !== undefined; } });
    return res;
  }

  var pm = {
    environment: envShim,
    globals: envShim,
    variables: envShim,
    collectionVariables: envShim,

    test: function (name, fn) { return recordTest(name, fn); },
    expect: chai,

    sendRequest: function (opts, cb) {
      var res, err = null;
      try {
        res = postmanResponse(sw.sendRequest(fromPostmanRequest(opts)));
      } catch (e) {
        err = e;
      }
      if (typeof cb === 'function') return cb(err, res);
      if (err) throw err;
      return res;
    },

    get info() {
      return {
        requestName: __swarmo.request ? __swarmo.request.name : '',
        eventName: __swarmo.response ? 'test' : 'prerequest'
      };
    },

    get request() { return __swarmo.request; },

    get response() {
      var r = __swarmo.response;
      if (!r) return undefined;
      var shim = {
        code: r.status,
        status: r.statusText,
        responseTime: r.durationMs,
        headers: {
          get: function (name) { return r.headers[String(name).toLowerCase()]; },
          has: function (name) { return r.headers[String(name).toLowerCase()] !== undefined; }
        },
        json: r.json,
        text: r.text,
        to: chai(r, true)
      };
      return shim;
    },

    cookies: unsupportedObject('pm.cookies', ['get', 'has', 'toObject', 'jar']),
    iterationData: unsupportedObject('pm.iterationData', ['get', 'has', 'toObject']),
    visualizer: unsupportedObject('pm.visualizer', ['set']),
    vault: unsupportedObject('pm.vault', ['get', 'set']),
    execution: unsupportedObject('pm.execution', ['setNextRequest', 'skipRequest'])
  };

  global.postman = {
    setNextRequest: unsupported('postman.setNextRequest'),
    setEnvironmentVariable: function (k, v) { setVar(k, v); },
    getEnvironmentVariable: function (k) { return lookup(k); },
    setGlobalVariable: function (k, v) { setVar(k, v); },
    getGlobalVariable: function (k) { return lookup(k); }
  };

  // ---------------------------------------------------------------------
  // ctx  (virtual-user API for load scripts)
  // ---------------------------------------------------------------------

  function recordCheck(name, passed) {
    var full = __swarmo.groupStack.length
      ? __swarmo.groupStack.join(' :: ') + ' :: ' + name
      : name;
    var e = __swarmo.checks[full];
    if (!e) { e = { passes: 0, fails: 0 }; __swarmo.checks[full] = e; }
    if (passed) e.passes++; else e.fails++;
    return passed;
  }

  function taggedUrl(tag, url) {
    if (tag) {
      return __swarmo.groupStack.length ? __swarmo.groupStack.join(' :: ') + ' :: ' + tag : tag;
    }
    return null;
  }

  __swarmo.makeCtx = function (vuId) {
    var vuState = { id: vuId, iteration: 0 };
    var vars = {};

    function req(method) {
      return function (url, opts) {
        opts = opts || {};
        var interpolated = __swarmo_interp(String(url));
        var tag = opts.tag || (String(method).toUpperCase() + ' ' + String(url));
        var o = {
          headers: opts.headers,
          params: opts.params,
          json: opts.json,
          body: opts.body,
          form: opts.form,
          tag: taggedUrl(tag, interpolated) || tag
        };
        // Interpolate header values too. Headers may be an object or
        // [name, value] pairs, like sw.sendRequest's.
        if (o.headers) {
          var pairs = headersToPairs(o.headers);
          for (var i = 0; i < pairs.length; i++) pairs[i][1] = __swarmo_interp(pairs[i][1]);
          o.headers = pairs;
        }
        if (typeof o.body === 'string') o.body = __swarmo_interp(o.body);
        return doRequest(method, interpolated, o);
      };
    }

    /** One unary gRPC call. Mirrors ctx.http's ergonomics. */
    function grpcCall(address, fullMethod, opts) {
      opts = opts || {};

      var target = String(fullMethod || '');
      var slash = target.lastIndexOf('/');
      if (slash <= 0) {
        throw new Error(
          'Expected "package.Service/Method" but got "' + target + '"'
        );
      }
      var service = target.slice(0, slash);
      var method = target.slice(slash + 1);

      var message = opts.message;
      if (message === undefined || message === null) message = {};
      var messageJson =
        typeof message === 'string'
          ? __swarmo_interp(message)
          : JSON.stringify(message);

      var metadata = [];
      if (opts.metadata) {
        for (var k in opts.metadata) {
          if (Object.prototype.hasOwnProperty.call(opts.metadata, k)) {
            metadata.push([k, __swarmo_interp(String(opts.metadata[k]))]);
          }
        }
      }

      var tag = opts.tag || service.split('.').pop() + '/' + method;
      if (__swarmo.groupStack.length) {
        tag = __swarmo.groupStack.join(' :: ') + ' :: ' + tag;
      }

      var payload = {
        address: __swarmo_interp(String(address)),
        service: service,
        method: method,
        metadata: metadata,
        messageJson: messageJson,
        tag: tag
      };

      var parsed = JSON.parse(__swarmo_grpc(JSON.stringify(payload)));
      if (!parsed.ok) throw new Error(parsed.error || 'the gRPC call failed');

      var raw = parsed.res;
      var res = {
        code: raw.code,
        codeName: raw.codeName,
        ok: raw.code === 0,
        statusMessage: raw.statusMessage || '',
        body: raw.body || '',
        headers: pairsToObject(raw.headers),
        trailers: pairsToObject(raw.trailers),
        durationMs: raw.durationMs || 0
      };
      res.text = function () { return res.body; };
      res.json = function () {
        if (!res.body) {
          throw new Error(
            'there is no response message: the call returned ' +
              res.codeName +
              (res.statusMessage ? ' (' + res.statusMessage + ')' : '')
          );
        }
        try {
          return JSON.parse(res.body);
        } catch (e) {
          throw new Error('the response message was not valid JSON: ' + e.message);
        }
      };
      return res;
    }

    var ctx = {
      vu: vuState,
      vars: vars,
      grpc: {
        call: grpcCall,
        /** Sugar: ctx.grpc.unary(addr, "pkg.Svc/M", messageObject) */
        unary: function (address, fullMethod, message, opts) {
          var o = opts || {};
          o.message = message;
          return grpcCall(address, fullMethod, o);
        }
      },
      http: {
        get: req('GET'),
        post: req('POST'),
        put: req('PUT'),
        patch: req('PATCH'),
        delete: req('DELETE'),
        head: req('HEAD'),
        options: req('OPTIONS'),
        request: function (method, url, opts) { return req(method)(url, opts); }
      },
      env: function (k) {
        var v = lookup(k);
        return v === undefined ? '' : v;
      },
      sleep: function (a, b) {
        var ms;
        if (b === undefined) ms = (Number(a) || 0) * 1000;
        else {
          var lo = Number(a) || 0, hi = Number(b) || 0;
          ms = (lo + Math.random() * (hi - lo)) * 1000;
        }
        __swarmo_sleep(ms);
      },
      check: function (res, checks) {
        var all = true;
        for (var name in checks) {
          if (!Object.prototype.hasOwnProperty.call(checks, name)) continue;
          var passed = false;
          try { passed = !!checks[name](res); } catch (_) { passed = false; }
          recordCheck(name, passed);
          if (!passed) all = false;
        }
        return all;
      },
      group: function (name, fn) {
        var stack = __swarmo.groupStack;
        var depth = stack.length;
        stack.push(String(name));
        var ret;
        try {
          ret = fn();
        } catch (e) {
          stack.length = depth;
          throw e;
        }
        // An async group keeps its prefix until its promise settles, so
        // tags and checks after an `await` inside it are still prefixed.
        if (ret && typeof ret.then === 'function') {
          return ret.then(
            function (v) { stack.length = depth; return v; },
            function (e) { stack.length = depth; throw e; }
          );
        }
        stack.length = depth;
        return ret;
      },
      fail: function (msg) { throw new Error(msg || 'ctx.fail()'); },
      expect: expect,
      log: function () { __swarmo_log('log', joinArgs(arguments)); }
    };
    return ctx;
  };

  global.__swarmo = __swarmo;
  global.sw = sw;
  global.pm = pm;
  global.expect = chai;
})(globalThis);
