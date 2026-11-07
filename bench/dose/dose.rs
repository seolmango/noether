use std::io::Read;

fn main() {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text).unwrap();
    let product: i64 = text.split_whitespace().map(|n| n.parse::<i64>().unwrap()).product();
    println!("{product}");
}
