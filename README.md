<p align="center"><img src="site/logo.svg" width="112" alt="Noether 로고"></p>

# Noether

변수가 파동이고 메모리에 온도가 있는 프로그래밍 언어입니다. 이상한 건 규칙이고, 실행은 진짜입니다. Noether는 인터프리터가 아니라 네이티브 코드를 만드는 컴파일러로 동작합니다.

```text
fermion total
fermion i
fermion heater
boson   x @ 2048
boson   y @ 2048       # 같은 자리에 겹친 두 변수

|heater> <- -1         # 덮어쓰면 열이 난다. 차가우면 값이 일부만 읽힌다
|heater> <- 0
|heater> <- -1
|heater> <- 0

|i> <- 10
while <i|~64 > 0       # ~64는 읽기 창의 폭
 |x> <- <total|~64
 |y> <- <i|~64
 |total> <- <2048|~64  # 겹친 자리를 읽으면 합이 나온다
 lower i
emit <total|~64
```

## 컨셉

- **변수는 메모리 위에 퍼져 있습니다.** 읽기 창이 변수 폭보다 좁으면 값의 일부만 읽힙니다.
- **메모리에 온도가 있습니다.** 온도가 변수 폭을 정합니다. 차가우면 넓게 퍼져서 일부만 읽히고 이웃과 섞이며, 뜨거우면 값이 흔들립니다. 프로그램이 온도를 알맞게 유지해야 합니다.
- **정확함에는 값이 있습니다.** 창을 넓히면 차가워도 전부 읽히지만 열이 창 폭에 비례해서 나고, 이웃까지 읽힙니다. 값을 덮어쓰면 지운 비트만큼 열이 납니다.
- **덧셈 연산자가 없습니다.** 변수를 겹쳐 놓고 그 자리를 읽어야 합이 나옵니다. 곱셈과 나눗셈도 읽는 방법으로 만듭니다.
- **들여쓰기는 1, 4, 9, 16칸입니다.** 깊이 n은 n²칸이고, 깊은 코드일수록 더 뜨겁습니다.
- **대칭을 선언하면 빨라집니다.** 컴파일러가 선언을 검사하고, 통과하면 온도 계산을 지운 코드를 만듭니다. 대칭 블록 안의 읽기는 열도 나지 않습니다.

언어를 쓰는 방법은 [문법 문서](docs/syntax.md)에, 영감을 준 물리는 [개념 문서](docs/concepts.md)에 있습니다.

## 성능

컴파일러는 Cranelift로 x86-64 네이티브 코드를 만듭니다. 메모리 규칙을 그대로 지키는 일반 경로는 느리지만, 온도가 구간 안이면 일반 메모리 읽기에 가까운 코드로 실행합니다. 구간을 벗어나면 일반 경로로 떨어집니다.

| 작업 | 일반 경로 | 정적 분석 적용 | 대칭 선언 |
|---|---|---|---|
| 읽기와 감소를 반복(회당) | 9.0 ns | 1.0 ns | 0.25 ns |
| 겹침으로 벡터 덧셈(원소당) | 73 ns | 22 ns | 4.6 ns |

값이 흔들리는 읽기는 덮어쓰기를 포함해 회당 약 16 ns입니다. 노트북에서 프로세스 전체 시간을 재서 나눈 값이고, 기기 상태에 따라 두 배 가까이 달라집니다.

## 빌드

Rust와 Windows x64용 Visual Studio Build Tools가 필요합니다.

```bash
cargo build --release
```

```bash
target/release/noetherc.exe examples/sum.noe -o sum.exe
```

```bash
./sum.exe
```

링크 없이 바로 실행할 수도 있습니다.

```bash
target/release/noetherc.exe examples/sum.noe --run
```

테스트는 예제 전체를 네이티브와 실행 엔진 양쪽으로 돌려 결과를 비교합니다.

```bash
cargo test
```

## 온도 기록

`--thermo`를 붙여 빌드하면 프로그램이 틱별 온도를 CSV로 남기고, 끝날 때 요약을 출력합니다.

```bash
target/release/noetherc.exe examples/overheat.noe --thermo -o overheat.exe
```

```bash
./overheat.exe
```

```text
thermo: 18600 ticks, peak T 11245 (critical 4096), 1478 noisy reads, 15006 bits erased -> overheat.thermo.csv
```

`overheat.thermo.csv`는 `tick,tile,temperature` 형식입니다. 행이 약 100만 개를 넘으면 전체 기간에 고르게 솎아냅니다. 최고 온도와 집계 값은 솎아내도 정확합니다.

## 플레이그라운드

[site/](site/)는 브라우저에서 코드를 실행하고 온도 변화와 변수의 파동을 틱별로 보여주는 정적 페이지입니다. 터미널 창에서 입출력을 직접 주고받을 수 있습니다. 실행 엔진을 WebAssembly로 컴파일해서 쓰므로 서버가 필요 없습니다.

**GitHub Pages에 올리기:** 저장소를 GitHub에 푸시하고, 저장소의 Settings > Pages에서 Source를 GitHub Actions로 고릅니다. 이후 `main`에 푸시할 때마다 [워크플로](.github/workflows/pages.yml)가 빌드와 배포를 합니다.

**로컬에서 보기:**

```bash
rustup target add wasm32-unknown-unknown
```

```bash
cargo build --release -p noether-play --target wasm32-unknown-unknown
```

```bash
cp target/wasm32-unknown-unknown/release/noether_play.wasm site/noether.wasm
```

```bash
python -m http.server 8123 -d site
```
