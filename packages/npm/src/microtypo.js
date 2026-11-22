import { Engine, defaultMaxInputLength, initSync, version } from './glue.js';

const VERSION = '@VERSION@';
const MAX_ENTRIES = 100_000;

class MicroTypoError extends Error {
  static code = 'ERR_MICROTYPO';

  constructor(message, options = {}) {
    super(message, options.cause ? { cause: options.cause } : undefined);

    this.name = new.target.name;
    this.code = new.target.code;

    if (options.details !== undefined) {
      this.details = options.details;
    }
  }
}

class MicroTypoConfigError extends MicroTypoError {
  static code = 'ERR_MICROTYPO_CONFIG';
}

class MicroTypoInputError extends MicroTypoError {
  static code = 'ERR_MICROTYPO_INPUT';
}

class MicroTypoBudgetError extends MicroTypoError {
  static code = 'ERR_MICROTYPO_BUDGET';
}

class MicroTypoVersionError extends MicroTypoError {
  static code = 'ERR_MICROTYPO_VERSION';
}

function capacityError(message, details) {
  return new MicroTypoInputError(message, { details: { reason: 'capacity', ...details } });
}

const ERRORS = { MicroTypoError, MicroTypoConfigError, MicroTypoInputError, MicroTypoBudgetError };
const UNSAFE_PREFIX_RE = /["<>&\s]/;

function lift(error) {
  if (error === null || typeof error !== 'object' || error.engine !== true) {
    return error;
  }

  const Class = ERRORS[error.name];

  if (Class) {
    return new Class(error.message, 'details' in error ? { details: error.details } : {});
  }

  return error.name === 'TypeError' ? new TypeError(error.message) : new Error(error.message);
}

function toUnits(text) {
  const units = new Uint16Array(text.length);

  for (let i = 0; i < text.length; i++) {
    units[i] = text.charCodeAt(i);
  }

  return units;
}

function fromUnits(units) {
  let text = '';

  for (let i = 0; i < units.length; i += 0x2000) {
    text += String.fromCharCode.apply(null, units.subarray(i, i + 0x2000));
  }

  return text;
}

const notJson = (value) =>
  typeof value === 'function' || typeof value === 'symbol' || typeof value === 'bigint';

function toJson(value) {
  return JSON.stringify(value, (_, v) =>
    typeof v === 'string' ? v.toWellFormed() : notJson(v) ? null : v
  );
}

function checkConfig(config) {
  if (
    config !== undefined &&
    config !== null &&
    (typeof config !== 'object' || Array.isArray(config))
  ) {
    throw new MicroTypoConfigError(
      `invalid config: expected an object, got ${Array.isArray(config) ? 'an array' : typeof config}`,
      { details: { issues: ['config: expected an object'] } }
    );
  }
}

function readProcessOptions(options) {
  const changes =
    options !== null && typeof options === 'object' && Object.hasOwn(options, 'changes')
      ? options.changes
      : undefined;

  if (changes === undefined) {
    return false;
  }

  if (typeof changes !== 'boolean') {
    throw new MicroTypoConfigError('process() option "changes" must be a boolean', {
      details: { option: 'changes', receivedType: typeof changes }
    });
  }

  return changes;
}

function freezeGroup(group) {
  return Object.freeze({ ...group, rules: Object.freeze(group.rules.map(Object.freeze)) });
}

class MicroTypo {
  #engine;
  #processing = false;

  onCycleLimit = null;

  constructor(config) {
    checkConfig(config);

    try {
      this.#engine = new Engine(config == null ? undefined : toJson(config));
    } catch (error) {
      throw lift(error);
    }
  }

  #call(method, args) {
    try {
      this.#engine.call(method, toJson(args));
    } catch (error) {
      throw lift(error);
    }

    return this;
  }

  registerRuleGroup() {
    throw new TypeError(
      'registerRuleGroup: a rule group is JavaScript code, which the wasm engine cannot run'
    );
  }

  getRuleGroup(name) {
    const json = name ? this.#engine.ruleGroup(String(name)) : undefined;

    return json === undefined ? undefined : freezeGroup(JSON.parse(json));
  }

  listRuleGroups() {
    return Object.freeze(JSON.parse(this.#engine.ruleGroups()).map(freezeGroup));
  }

  addSafeTag(tag) {
    if (typeof tag === 'bigint') {
      JSON.stringify(tag);
    }

    const absent = tag === undefined || typeof tag === 'function' || typeof tag === 'symbol';

    return this.#call('addSafeTag', absent ? [] : [tag]);
  }

  addSafeBlock(spec) {
    let args = spec === undefined ? [] : [notJson(spec) ? 0 : spec];
    let spellError;

    if (spec !== null && typeof spec === 'object') {
      const { id, open, close, unsafeRegex } = spec;
      const plain =
        id == null ||
        typeof id === 'string' ||
        typeof id === 'boolean' ||
        (typeof id === 'number' && Number.isFinite(id));
      let spelled = id;

      if (!plain) {
        try {
          spelled = `${id}`;
        } catch (error) {
          spelled = undefined;
          spellError = { error };
        }
      }

      args = [{ id: spelled, open, close, unsafeRegex }];
    }

    try {
      return this.#call('addSafeBlock', args);
    } catch (error) {
      if (
        error instanceof MicroTypoConfigError &&
        error.message.startsWith("safe block '") &&
        error.details &&
        spec != null
      ) {
        if (spellError) {
          throw spellError.error;
        }

        const { id, open, close } = spec;

        Object.assign(error.details, 'open' in error.details ? { id, open, close } : { id });
      }

      throw error;
    }
  }

  setLayout(layout) {
    if (layout !== 'style' && layout !== 'class' && layout !== 'both') {
      throw new MicroTypoConfigError(
        `layout must be "style", "class", or "both": ${JSON.stringify(layout)}`,
        { details: { layout } }
      );
    }

    return this.#call('setLayout', [layout]);
  }

  setPrefix(p) {
    const prefix = p === true ? 'mt_' : p || '';
    const spelled = `${prefix}`;

    if (UNSAFE_PREFIX_RE.test(spelled)) {
      throw new MicroTypoConfigError(
        `prefix must not contain a quote, angle bracket, "&", or whitespace: ${JSON.stringify(prefix)}`,
        { details: { prefix } }
      );
    }

    return this.#call('setPrefix', [spelled]);
  }

  applyOptions(options) {
    checkConfig(options);

    return this.#call('applyOptions', options === undefined ? [] : [options]);
  }

  process(text, options) {
    if (typeof text !== 'string') {
      throw new MicroTypoInputError(`process() expects string, got ${typeof text}`, {
        details: { receivedType: typeof text }
      });
    }

    const withChanges = readProcessOptions(options);

    if (this.#processing) {
      throw new MicroTypoInputError(
        'process() called reentrantly on the same MicroTypo instance; recursive process() calls are not supported',
        { details: {} }
      );
    }

    const hook = typeof this.onCycleLimit === 'function' ? this.onCycleLimit : undefined;
    let units;
    let failure;

    this.#processing = true;

    try {
      try {
        units = this.#engine.process(toUnits(text), withChanges);
      } catch (error) {
        failure = { error: lift(error) };
      }

      if (hook) {
        for (const [id, n] of this.#engine.takeCycleLimits()) {
          hook.call(undefined, id, n);
        }
      }
    } finally {
      this.#processing = false;
    }

    if (failure) {
      throw failure.error;
    }

    const result = fromUnits(units);

    if (!withChanges) {
      return result;
    }

    const flat = this.#engine.takeChanges();
    const changes = [];

    for (let i = 0; i < flat.length; i += 2) {
      changes.push([flat[i], flat[i + 1]]);
    }

    return { text: result, changes };
  }

  free() {
    this.#engine.free();
  }
}

function microtypo(text, options) {
  return new MicroTypo(options).process(text);
}

async function compile(source) {
  if (source instanceof WebAssembly.Module) return source;
  if (typeof source === 'string') source = new URL(source, import.meta.url);
  if (source instanceof URL) {
    if (source.protocol === 'file:') {
      const { readFile } = await import('node:fs/promises');
      return WebAssembly.compile(await readFile(source));
    }
    source = fetch(source);
  }
  if (source instanceof Promise || source instanceof Response) {
    return WebAssembly.compileStreaming(source);
  }
  return WebAssembly.compile(source);
}

function mismatch(found) {
  return new MicroTypoVersionError(
    `microtypo.wasm is ${found === undefined ? 'from another build' : `version ${found}`}, microtypo.js is version ${VERSION}`,
    { details: { expected: VERSION, found } }
  );
}

async function instantiate(source) {
  const module = await compile(source);

  if (!WebAssembly.Module.exports(module).some(({ name }) => name === 'version')) {
    throw mismatch(undefined);
  }

  try {
    initSync({ module });
  } catch (error) {
    throw error instanceof WebAssembly.LinkError ? mismatch(undefined) : error;
  }

  const found = version();

  if (found !== VERSION) {
    throw mismatch(found);
  }

  return Object.freeze({
    MicroTypo,
    microtypo,
    VERSION,
    DEFAULT_MAX_INPUT_LENGTH: defaultMaxInputLength(),
    MAX_ENTRIES,
    capacityError,
    MicroTypoError,
    MicroTypoConfigError,
    MicroTypoInputError,
    MicroTypoBudgetError,
    MicroTypoVersionError
  });
}

let loading;

export function load(source = new URL('./microtypo.wasm', import.meta.url)) {
  loading ??= instantiate(source).catch((error) => {
    loading = undefined;
    throw error;
  });

  return loading;
}
