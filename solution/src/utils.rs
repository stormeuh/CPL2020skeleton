/// Credit to vitalyd [here](https://users.rust-lang.org/t/quotient-and-remainder/16093) for this implementation
/// Compiles to more efficient machine code than doing floor and % separately
pub fn div_rem<T: std::ops::Div<Output=T> + std::ops::Rem<Output=T> + Copy>(x: T, y: T) -> (T, T) {
    let quot = x / y;
    let rem = x % y;
    (quot, rem)
}
