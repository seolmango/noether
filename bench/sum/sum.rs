use std::hint::black_box;

// 최적화를 켜면 Rust는 반복문을 지우고 답만 내놓습니다. 일을 시켜야 합니다.
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 1;
    while i <= 100_000_000 {
        total = black_box(total + i);
        i += 1;
    }
    println!("{total}");
}
