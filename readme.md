# microtypo.rs

Движок русской типографики на Rust: кавычки-«ёлочки», тире, неразрывные пробелы, знаки, абзацы. Защищает код, URL и разметку. Порт [microtypo](https://github.com/meritt/microtypo) с тем же конфигом и API; результат совпадает байт в байт.

## Rust

```toml
[dependencies]
microtypo-core = { git = "https://github.com/meritt/microtypo.rs", tag = "v0.2.0" }
getrandom = "0.4"
serde_json = "1"
```

Часы для `maxProcessingMs` и случайные байты движок берёт у вызывающего кода через `Host`.

```rust
use std::sync::LazyLock;
use std::time::Instant;

use microtypo_core::{Host, MicroTypo};
use serde_json::json;

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

let host = Host {
    now_ms: || START.elapsed().as_secs_f64() * 1000.0,
    fill_random: |bytes| getrandom::fill(bytes).expect("системный CSPRNG доступен"),
};
let mut typo = MicroTypo::new(Some(&json!({ "html": false })), host)?;
assert_eq!(typo.process("\"Амбер\" - город")?, "«Амбер»\u{a0}— город");
```

## JavaScript

```bash
pnpm add @meritt/microtypo-wasm
```

```js
import { load } from '@meritt/microtypo-wasm';

const { MicroTypo } = await load();
const typo = new MicroTypo({ html: false });
typo.process('"Амбер" - город'); // '«Амбер» — город'
```

`load()` берёт `microtypo.wasm` рядом с модулем; можно передать URL, `Response`, байты или `WebAssembly.Module`. После успешной загрузки модуль один на realm: следующие вызовы возвращают тот же объект и аргумент не читают.

## Swift

```swift
.package(url: "https://github.com/meritt/microtypo.rs", exact: "0.2.0"),
```

```swift
import MicroTypo

let typo = try MicroTypo(config: .init(html: false, rules: ["quote": false]))
let text = try typo.process("\"Амбер\" - город")
```

`MicroTypo.Config` повторяет поля конфига ядра; `rules` применяются в порядке записи. Ошибки движка приходят как `MicroTypoError`, неконечный `maxProcessingMs` отклоняется `EncodingError`. Экземпляр `Sendable`.

## Автор

[Алексей Симоненко](https://simonenko.xyz)

## Лицензия

MIT. Смотрите файл `LICENSE`.
