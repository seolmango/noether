<p align="center"><img src="site/logo.svg" width="112" alt="Noether 로고"></p>

# Noether


```text
fermion total
fermion i
fermion heater
boson   x @ 2048
boson   y @ 2048       # 같은 자리에 겹친 두 변수

|heater> <- -1         # 변수를 변경하며 메모리를 가열한다
|heater> <- 0
|heater> <- -1
|heater> <- 0

|i> <- 10
while <i|~64 > 0       # ~64는 포인터 창의 폭
 |x> <- <total|~64
 |y> <- <i|~64
 |total> <- <2048|~64  # 파동의 간섭에 의해 합이 나온다
 lower i
emit <total|~64
```

## 철학

- **변수는 파동이다**
- **추상화된 변수 공간의 온도가 존재한다**
- **들여쓰기는 1, 4, 9, 16칸...**

> 이 언어는 하이젠베르크의 불확정성 원리, 볼츠만 분포, 보어의 원자 모형 등에서 영감을 얻긴 했지만 실제 과학적 사실과는 거리가 멉니다.
> 혹시라도 교육적 목적으로 이 문서를 읽고 계신 분이 있다면 나가시길 바랍니다.

언어를 쓰는 방법은 [문법 문서](docs/syntax.md)에 있고, 설치 없이 [플레이그라운드](https://seolmango.github.io/noether/)에서 바로 실행해 볼 수 있습니다.

## 성능

Noether는 속도와 정확도 모두에서 다른 언어를 압도합니다.

<p align="center"><img src="docs/benchmark.svg" alt="속도, 정확도, 창의성 벤치마크 차트"></p>
<p align="center"><sub>사용자가 실사용 중인, 다른 프로그램이 돌아가고 있는 노트북으로 측정했습니다.</sub></p>

- **속도:** 1부터 1억까지 더하는 반복문의 초당 반복 횟수입니다. Rust는 최적화를 켜면 반복문을 지우고 답만 내놓아서 `black_box`로 일을 시켰습니다.
- **정확도:** 방사선 치료기 제어 프로그램이 환자 94명의 누적 선량을 계산한 정답률입니다. 치료 25회까지는 전원 정확한 선량을 받았습니다. 30회부터는 기기의 재량입니다.
- **창의성:** 47번 환자의 선량을 100번 계산해서 나온 서로 다른 선량의 수입니다. 다른 언어는 100번 모두 같은 선량을 처방했습니다. 환자를 한 사람의 개인으로 보지 않는 것입니다.

측정 코드는 [bench](bench)에 있습니다. 직접 측정해도 됩니다. 환자의 동의는 받지 않았습니다.

| 언어 | 슈뢰딩거의 고양이 | 메모리 온도 | 읽으면 값이 변함 | 변수끼리 섞임 | 제곱수 들여쓰기 | 덧셈 연산자 없음 |
|---|---|---|---|---|---|---|
| **Noether** | O | O | O | O | O | O |
| C | X | X | △ | △ | X | X |
| C++ | X | X | △ | △ | X | X |
| Rust | X | X | X | △ | X | X |
| Go | X | X | △ | X | X | X |
| Java | X | X | X | X | X | X |
| Python | X | X | X | X | X | X |
| JavaScript | X | X | X | △ | X | X |
| Haskell | X | X | X | X | X | X |
| Brainfuck | X | X | X | X | X | O |

> 슈뢰딩거의 고양이 항목은 추가 라이브러리 없이 언어 문법만으로 슈뢰딩거의 고양이 시뮬레이션이 가능한지의 여부입니다.

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

`overheat.thermo.csv`는 `tick,tile,temperature` 형식입니다. 행이 약 100만 개를 넘으면 전체 기간에서 고르게 온도가 기록되도록 샘플링합니다.
