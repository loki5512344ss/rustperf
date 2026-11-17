# План доработок форка (2026-11-12)

Работа велась в ветке `fork/harness-gates-and-catalog`, затем влита в `main`. Статусы обновляются по мере работы:
`[ ]` не начато, `[~]` в работе, `[x]` сделано и проверено.

Правило для всего плана: число без команды, которая его дала, не записывается.
Всё, что не проверено на этой машине (rustc 1.99.1, Xeon E5-2680 v3), помечается
`not verified`.

## A. Харнесс (`src/`) — форк перестаёт быть «upstream без изменений»

- [x] A1. Проверка на `unsafe`. Новый гейт: кандидат не может добавить или
  изменить `unsafe`-регион сверх того, что было в базовом коммите. Новая причина
  `FAIL(unsafe_added)`. Выключается только человеком через конфиг
  (`allow_unsafe: true`), конфиг заперт хешем.
  Реализовано не по исходной формулировке: разбор идёт по токенам через
  `proc_macro2` (не `syn`), и правило регионное, а не списочное — `get_unchecked`
  ловится потому, что обязан лежать внутри нового или изменённого региона;
  списочное правило пропускало бы следующее имя. Старые имена (`unsafe`-блок,
  `unsafe fn`, `unsafe impl`) — это формы региона. Проверено:
  `src/unsafe_gate.rs` (23 юнит-теста в `cargo test --lib`, 243 lib-теста
  всего), 6 интеграционных тестов гейта в `tests/pipeline_gates.rs`
  (23 passed, включая «гейт срабатывает до сборки» — `target/` не появляется,
  и переименование файла с неизменённым `unsafe`).
- [x] A2. Дельты по каждому бенчмарку в `eval --json`: имя, медианы, `pct_change`,
  `p`, признак значимости — на KEEP и на DISCARD, а не только при сработавшей защите.
  Проверено: `cargo test --test cmd_eval -- --ignored` — 4 passed (129 с),
  включая KEEP и DISCARD с непустым массивом `benchmarks`.
- [x] A3. Счёт инструкций как вторая метрика. Сначала скриптом (B1), затем решение,
  встраивать ли в `eval` (зависит от того, работает ли `perf stat` под харнессом).
  `perf stat` работает; решение: остаётся скриптом, в `eval` не встраивается —
  score должен остаться одним сравнимым числом, а PMU есть не на каждом runner.
  Записано в `docs/fork-notes.md`.
- [x] A4. Обновить `harness-cli.md`, `fork-notes.md`, `README` (раздел про форк),
  тесты на каждый новый гейт.
  `harness-cli.md`, `fork-notes.md`, `templates/program.md`, `README.md`
  (включая «Fork changes» и счёт 303 из `cargo test --all-targets -- --list`)
  и `docs/rust_guide.md` §48.5–48.7 обновлены; тесты — A1/A2.

## B. Скрипты (`scripts/`)

- [x] B1. `instr-count.sh` — A/B двух команд по `perf stat -e instructions:u,cycles:u`,
  чередование, медианы, отказ при недоступном `perf`.
- [x] B2. `layout-check.sh` — контроль случайной раскладки кода: повтор A/B при
  изменённом окружении (длина env, порядок линковки), вывод «эффект устойчив / нет».
- [x] B3. `vec-remarks.sh` — замечания LLVM о векторизации для одной функции
  (`-C remark=loop-vectorize`), без чтения asm.
- [x] B4. `diff-fuzz.sh` — заготовка дифференциального фаззинга «старая функция
  против новой» (без внешних зависимостей; свой xorshift, граничные размеры).
- [x] B5. `perf-diff.sh` — `perf diff` двух записей; при недоступном `perf record`
  честный отказ с подсказкой.
  Проверено 2026-11-12 прогоном всех шести скриптов на `docs/rust_perf_case`:
  обычный путь и доступные отказы; найдены и исправлены две ошибки —
  hyperfine 2.0 по умолчанию `--shell=none` (молча ломал `a && b`) и локаль
  в CSV `perf` (ломала разбор при `ru_RU`). Детали:
  `docs/rust_perf_case/RESULTS.md`, раздел «Fork scripts, run on 2026-11-12».

## C. Каталог техник (`skills/.../optimization-catalog.md`)

- [x] C1. Новые строки: раскладка Эйцингера и безветвенный бинарный поиск; radix
  sort; сортирующие сети; SWAR; фильтр Блума; `phf`; `spare_capacity_mut` /
  `MaybeUninit`; `hint::assert_unchecked`, `select_unpredictable`, `cold_path`
  (статус в 1.99.1 проверить компиляцией); `collect` с переиспользованием
  аллокации; `size_hint`; `itoa`/`ryu`/`lexical`; huge pages и `madvise`;
  non-temporal записи; `io_uring`; индексы `u32`; `arc-swap`; `x86-64-v3`;
  мультиверсии функций.
  Проверено: строки на месте; статусы `hint::*` в таблице MSRV — «stable on
  1.99.1 (verified by compiling)».
- [x] C2. Вычистка дублей (`b37-*`, `t*`, `ex19-*` против `b*`, `a*`, `c*`),
  честный пересчёт числа техник в заголовке.
  Проверено подсчётом: 303 строки в секциях техник, дублей ID нет;
  заголовок «303 distinct techniques» совпадает.

## D. Справочники скилла и гайд

- [x] D1. `measurement.md`: счёт инструкций, контроль раскладки, регрессии в CI.
- [x] D2. `profiling.md`: `perf diff`, дифференциальные flame graph, замечания LLVM.
- [x] D3. `semantics-traps.md`: Kani, дифференциальный фаззинг.
- [x] D4. `docs/rust_guide.md`: новый раздел §48 с тем же материалом по-русски.
  §48 есть; §48.7 обновлён по фактическим прогонам скриптов и гейта.
- [x] D5. `SKILL.md`: ссылки на новые скрипты и разделы.

## Кто что делает

| Часть | Исполнитель |
|---|---|
| A1, A2, A4, D5, сведение | основной агент |
| B1–B5 | субагент (скрипты, проверка на `docs/rust_perf_case`) |
| C1–C2 | субагент (каталог, проверка API компиляцией) |
| D1–D4 | субагент (справочники и гайд) |

Файлы между исполнителями не пересекаются. Коммиты — только по команде владельца.

## E. Харнесс: закрытие известных дыр (2026-11-15)

- [x] E1. Заморозка `tests/` и `benches/` у каждого пакета (любой каталог с
  `Cargo.toml`), а не только у корня workspace. Проверено: юнит-тест
  `frozen_test_files_reach_workspace_members_at_any_depth`.
- [x] E2. Baseline-worktree должен быть чистым, а не только стоять на нужном
  коммите: `FAIL(baseline_tampered)`. Проверено: интеграционный тест
  `an_uncommitted_edit_in_the_baseline_worktree_is_detected`.
- [x] E3. Gitignored `.rs` вне scope хешируются на baseline и сверяются при
  каждом `eval`. Проверено: `editing_a_gitignored_source_outside_the_scope_fails`.
- [x] E4. Защита от регрессий срабатывает и на медианном замедлении от 50 % при
  любом `p`. Проверено: два юнит-теста в `verdict.rs`.
- [x] E5. `baseline` предупреждает, если `count` и число бенчмарков делают KEEP
  недостижимым. Проверено: два юнит-теста в `cmd_baseline.rs`.
- [x] E6. `status --json`, `report --json`. Проверено: по интеграционному тесту.
- [ ] E7. Не сделано: гейты clippy и Miri, метрика аллокаций, закрытие
  оставшихся обходов unsafe-гейта (существующий unsafe-макрос, не-`.rs`
  include, `build.rs`). Причины и варианты — в `docs/fork-notes.md`.

## F. Дешёвый батч по итогам Rust 1.100 (2026-11-17)

Основание: `~/Downloads/rust-1.100-changelog-rustperf.md` §5.9–5.10 и
`videoplayback.txt`. Крупные пункты (PGO-режим, телеметрия аллокаций,
лид-детектор) отложены: они меняют контракт `eval --json`.

Правила батча: один агент — одна задача; файлы задач не пересекаются;
агент не коммитит и не правит README/`templates/program.md`/этот план
(их сводит основной агент); новые API Rust 1.100 не использовать, пока
не решено поднимать MSRV с 1.85.

- [x] F1. `baseline` прогревает pinned worktree: после `gitx::add_worktree`
  выполнить `cargo build --release --all-targets` в worktree с таймаутом из
  конфига. Провал прогрева — предупреждение, не отказ: `baseline` не должен
  падать из-за окружения, `eval` всё равно сообщит о сломанной сборке.
  Файлы: `src/bin/rustperf/cmd_baseline.rs` (+ тесты там же).
- [x] F2. `doctor`: две новые проверки. (а) `dependency cooldown` — задан ли
  `[registry] global-min-publish-age` в `.cargo/config.toml` репозитория или
  в конфиге `CARGO_HOME`/`~/.cargo`; отсутствие — `warn` с предложением
  `3 days`. (б) `toolchain age` — по `rustc -Vv` (`release`, `commit-date`):
  свежее 7 дней — `warn` про свежую кодогенерацию; nightly — подсказка про
  `--jobs-frontend`. Разбор вынесен в чистые функции, тесты не запускают
  `rustc`. Файлы: `src/doctor.rs`.
- [x] F3. Колонка `version` в `results.tsv`: `HEADER` получает шестое поле,
  `Row` — `version: String`. `load` принимает и 5 полей (старые логи,
  `version` пустой), и 6; `append` пишет 6. `eval` пишет версию бинарника.
  Файлы: `src/results.rs`, `src/bin/rustperf/cmd_eval.rs` (+ тесты).
- [x] F4. `stop --force`: после `FORCE_GRACE` без освобождения claim —
  эскалация. `Runner` записывает pid запущенной команды (групповой лидер,
  см. `procgroup_unix::configure`) в файл в `std::env::temp_dir()`, имя
  содержит pid самого `eval`; после `wait` — удаляет. `stop --force` при
  таймауте шлёт `SIGKILL` в записанную группу, затем в сам `eval`, и честно
  очищает claim. Только unix; на прочих платформах поведение не меняется.
  Проверяется юнит-тестами на путь/разбор; живой «зависший eval» в CI не
  воспроизводится — это признаётся в отчёте.
  Файлы: `src/runner.rs`, `src/procgroup_unix.rs`, `src/procgroup_other.rs`,
  `src/state.rs` (или новый маленький модуль), `src/bin/rustperf/cmd_stop.rs`.
- [x] F5. CI: недельный `schedule` (cron) + `workflow_dispatch`, отдельная
  job на ubuntu, которая гоняет 4 `#[ignore]`-теста:
  `cargo test --test cmd_eval -- --ignored`. Обычные push/PR job не меняются.
  Файлы: `.github/workflows/ci.yml`.
- [x] F6. Банк идей `templates/program.md`: добавить проверенные на 1.100.0
  API (`trim_prefix`/`trim_suffix` для `str`/`[T]`/`Path`, `Vec::from_fn`,
  `bool::toggle`, `Box::map`/`Arc::map`, `Result<T, !>`), с оговорками:
  MSRV репозитория, арена на `Allocator` — только с замером и с временем
  жизни, покрывающим все использования. Владелец: основной агент.
- [x] F7. Свести доки: README (ограничения, fork changes, число тестов),
  `docs/fork-notes.md`, `templates/program.md` (колонки `results.tsv`).
  Владелец: основной агент.
