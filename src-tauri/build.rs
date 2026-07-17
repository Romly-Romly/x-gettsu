fn main() {
    // アイコン差し替え時に build script の再実行(=リソース再コンパイル)を保証するため、icons ディレクトリを変更監視の対象へ加える
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
