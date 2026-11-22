# microtypo.rs

Движок русской типографики на Rust: кавычки-«ёлочки», тире, неразрывные пробелы, знаки, абзацы. Защищает код, URL и разметку. Порт [microtypo](https://github.com/meritt/microtypo) с тем же конфигом и API; результат совпадает байт в байт.

## Rust

```toml
[dependencies]
microtypo-core = { git = "https://github.com/meritt/microtypo.rs", tag = "v0.1.0" }
```

```rust
use microtypo_core::MicroTypo;
use serde_json::json;

let mut typo = MicroTypo::new(Some(&json!({ "html": false })))?;
assert_eq!(typo.process_str("\"Амбер\" - город")?, "«Амбер»\u{a0}— город");
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
.package(url: "https://github.com/meritt/microtypo.rs", exact: "0.1.0"),
```

```swift
import MicroTypo

let typo = try MicroTypo(config: ["html": false])
let text = try typo.process("\"Амбер\" - город")
```

## Автор

[Алексей Симоненко](https://simonenko.xyz)

## Лицензия

MIT. Смотрите файл `LICENSE`.
