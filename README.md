<p align="center"><img src="site/logo.svg" width="112" alt="Noether 로고"></p>

# Noether

변수가 파동이고 메모리에 온도가 있는 프로그래밍 언어입니다. 이상한 건 규칙이고, 실행은 진짜입니다. Noether는 인터프리터가 아니라 네이티브 코드를 만드는 컴파일러로 동작합니다.

```text
fermion total @ 0
fermion i     @ 4096
boson   x     @ 8192
boson   y     @ 8192       # 같은 자리에 겹친 두 변수

|i> <- 10
while <i|~32 > 0           # ~32는 읽기 창의 폭
 |x> <- <total|~32
 |y> <- <i|~32
 |total> <- <8192|~32      # 겹친 자리를 읽으면 합이 나온다
 lower i
emit <total|~32
```

## 컨셉

- **변수는 메모리 위에 퍼져 있습니다.** 넓은 창으로 읽으면 값이 정확하지만 이웃까지 읽힐 수 있고, 좁은 창으로 읽으면 값이 흔들립니다.
- **덧셈 연산자가 없습니다.** 변수를 겹쳐 놓고 그 자리를 읽어야 합이 나옵니다. 곱셈과 나눗셈도 읽는 방법으로 만듭니다.
- **메모리에 온도가 있습니다.** 값을 덮어쓰면 지운 비트만큼 열이 나고, 임계 온도를 넘으면 값이 흔들립니다. 차가워지면 근처 변수가 섞입니다.
- **들여쓰기는 1, 4, 9, 16칸입니다.** 깊이 n은 n²칸이고, 깊은 코드일수록 더 뜨겁습니다.
- **대칭을 선언하면 빨라집니다.** 컴파일러가 선언을 검사하고, 통과하면 온도 계산을 지운 코드를 만듭니다.

언어를 쓰는 방법은 [문법 문서](docs/syntax.md)에, 영감을 준 물리는 [개념 문서](docs/concepts.md)에 있습니다.

## 성능

컴파일러는 Cranelift로 x86-64 네이티브 코드를 만듭니다. 메모리 규칙을 그대로 지키는 일반 경로는 느리지만, 물리가 결과에 영향을 줄 수 없다고 증명되면 그 비용을 지웁니다.

| 작업 | 일반 경로 | 정적 분석 적용 | 대칭 선언 |
|---|---|---|---|
| 읽기와 감소를 반복(회당) | 8.3 ns | 1.7 ns | 0.5 ns |
| 겹침으로 벡터 덧셈(원소당) | 197 ns | 35 ns | 9 ns |

값이 흔들리는 읽기는 덮어쓰기를 포함해 회당 약 35 ns입니다. 노트북에서 프로세스 전체 시간을 재서 나눈 값이고, 기기 상태에 따라 두 배 가까이 달라집니다.

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
thermo: 16418 ticks, peak T 10425 (critical 4096), 12 noisy reads, 13954 bits erased -> overheat.thermo.csv
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
