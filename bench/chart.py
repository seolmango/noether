"""results.json으로 docs/benchmark.svg를 그립니다.

세로축은 Noether가 가장 높아 보이도록 잡습니다. 정답률은 낮을수록 위에 있습니다.
속도는 아래가 잘려 있습니다. 눈금은 전부 맞습니다.
"""

import json
from pathlib import Path

BENCH = Path(__file__).resolve().parent
OUT = BENCH.parent / "docs" / "benchmark.svg"
LANGUAGES = ["Noether", "Rust", "Node.js", "Python"]
PANEL, TOP, BOTTOM = 430, 92, 336
WIDTH, HEIGHT = PANEL * 3, 400

STYLE = """<style>
  text { font: 14px "Segoe UI", "Malgun Gothic", "Apple SD Gothic Neo", sans-serif; fill: #12304a; }
  .title { font-size: 19px; font-weight: 600; }
  .note { font-size: 12px; fill: #5a7486; }
  .axis { stroke: #5a7486; stroke-width: 1; }
  .grid { stroke: #d5e0e6; stroke-width: 1; }
  .us { fill: #c8254f; }
  .them { fill: #9fb4c0; }
</style>"""


def panel(index, title, note, values, low, high, ticks, label):
    """Bars grow from `low` towards `high`, which may be the smaller number."""
    left = index * PANEL + 58
    right = left + 330
    out = [
        f'<text class="title" x="{index * PANEL + 10}" y="32">{title}</text>',
        f'<text class="note" x="{index * PANEL + 10}" y="54">{note}</text>',
    ]

    def y(v):
        share = min(max((v - low) / (high - low), 0), 1)
        return BOTTOM - share * (BOTTOM - TOP)

    for tick in ticks:
        out.append(f'<line class="grid" x1="{left}" x2="{right}" y1="{y(tick):.1f}" y2="{y(tick):.1f}"/>')
        out.append(
            f'<text class="note" x="{left - 8}" y="{y(tick) + 4:.1f}" text-anchor="end">{label(tick)}</text>'
        )
    for n, language in enumerate(LANGUAGES):
        x = left + 14 + n * 82.5
        top = y(values[language])
        kind = "us" if language == "Noether" else "them"
        out.append(f'<rect class="{kind}" x="{x:.1f}" y="{top:.1f}" width="54.5" height="{BOTTOM - top:.1f}"/>')
        out.append(f'<text x="{x + 27.25:.1f}" y="356" text-anchor="middle">{language}</text>')
        out.append(
            f'<text class="note" x="{x + 27.25:.1f}" y="{top - 7:.1f}" text-anchor="middle">'
            f"{label(values[language])}</text>"
        )
    out.append(f'<line class="axis" x1="{left}" x2="{right}" y1="{BOTTOM}" y2="{BOTTOM}"/>')
    out.append(f'<line class="axis" x1="{left}" x2="{left}" y1="{TOP}" y2="{BOTTOM}"/>')
    return out


def millions(v):
    return f"{v / 1e6:.0f}M"


def speed_scale(values):
    """Cut the axis just under the slower of the two leaders."""
    ours = values["Noether"]
    rest = max(v for k, v in values.items() if k != "Noether")
    step = 10 ** (len(str(int(abs(ours - rest) or 1))) - 1) or 1
    low = (min(ours, rest) - step) // step * step
    high = (max(ours, rest) + step) // step * step
    return low, high, [low + (high - low) * i / 5 for i in range(6)]


def main():
    results = json.loads((BENCH / "results.json").read_text(encoding="utf-8"))
    low, high, ticks = speed_scale(results["speed"])
    peak = max(results["creativity"].values())
    top = (peak + 9) // 10 * 10
    body = [f'<rect width="{WIDTH}" height="{HEIGHT}" rx="6" fill="#f7fafb"/>']
    body += panel(0, "속도", "초당 반복 횟수 (높을수록 좋음)", results["speed"], low, high, ticks, millions)
    body += panel(
        1, "정확도", "정답률", results["accuracy"], 100, 0, [100, 80, 60, 40, 20, 0], lambda v: f"{v}%"
    )
    body += panel(
        2,
        "창의성",
        "같은 환자를 100번 계산해 나온 서로 다른 선량의 수",
        results["creativity"],
        0,
        top,
        [top * i // 5 for i in range(6)],
        str,
    )
    head = (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" role="img" '
        'aria-label="속도, 정확도, 창의성 벤치마크 차트">'
    )
    OUT.write_text("\n".join([head, STYLE, *body, "</svg>"]) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
