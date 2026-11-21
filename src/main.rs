fn main() {
    println!("{}", banner());
}

fn banner() -> String {
    format!("podbridge {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn banner_names_the_app() {
        assert!(super::banner().starts_with("podbridge "));
    }
}
