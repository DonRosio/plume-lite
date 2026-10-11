//! Updates: the client downloads the build of its platform, checks its SHA-256 and its
//! minisign signature against the public key built into it, and only then keeps the file.
//! A server that was broken into cannot hand out a modified client this way: the signing key
//! never leaves the release workflow.

use std::path::{Path, PathBuf};

use minisign_verify::{PublicKey, Signature};
use plume_lite_protocol::http::ClientDownload;
use sha2::{Digest, Sha256};

use crate::api::Api;

/// The public half of the key that signs releases (`minisign.pub` at the root of the repo).
pub const PUBLIC_KEY: &str = include_str!("../../../minisign.pub");

/// Checks a downloaded build. The errors are shown to the user.
pub fn verify(data: &[u8], sha256: &str, signature: &str, public_key: &str) -> Result<(), &'static str> {
    let digest: String = Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect();
    if !digest.eq_ignore_ascii_case(sha256) {
        return Err("Le fichier téléchargé est incomplet ou a été modifié.");
    }
    let key = PublicKey::decode(public_key).map_err(|_| "La clé de vérification du client est illisible.")?;
    let signature = Signature::decode(signature).map_err(|_| "La signature de la mise à jour est illisible.")?;
    key.verify(data, &signature, false)
        .map_err(|_| "La mise à jour n'est pas signée par Plume Lite : elle a été écartée.")
}

/// Downloads `download`, checks it, and saves it in the user's download folder. Returns
/// where it is.
pub async fn download(api: Api, version: String, download: ClientDownload) -> Result<PathBuf, String> {
    let signature_path = download.signature_path.as_deref().ok_or("Cette mise à jour n'est pas signée.")?;
    let data = api.file(&download.path).await.map_err(|err| err.message)?;
    let signature = api.file(signature_path).await.map_err(|err| err.message)?;
    let signature = String::from_utf8(signature).map_err(|_| "La signature de la mise à jour est illisible.")?;
    verify(&data, &download.sha256, &signature, PUBLIC_KEY)?;

    let folder = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
        .ok_or("Dossier de téléchargement introuvable.")?;
    let path = folder.join(versioned_name(&download.path, &version));
    // Written next to its final place, then renamed: never a half-written client.
    let partial = path.with_extension("part");
    tokio::fs::write(&partial, &data).await.map_err(|err| format!("Enregistrement impossible : {err}"))?;
    tokio::fs::rename(&partial, &path).await.map_err(|err| format!("Enregistrement impossible : {err}"))?;
    Ok(path)
}

/// `/download/plume-lite.exe` → `plume-lite-0.5.1.exe`: the running client may sit in the
/// same folder under the plain name (and Windows cannot overwrite a running program).
fn versioned_name(path: &str, version: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or("plume-lite");
    let name = name.replace(['/', '\\'], "");
    match name.split_once('.') {
        Some((stem, extension)) => format!("{stem}-{version}.{extension}"),
        None => format!("{name}-{version}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn digest(data: &[u8]) -> String {
        Sha256::digest(data).iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn only_signed_and_intact_builds_pass() {
        let minisign::KeyPair { pk, sk } = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let public_key = pk.to_box().unwrap().to_string();
        let data = b"plume-lite 0.5.1".to_vec();
        let signature = minisign::sign(Some(&pk), &sk, Cursor::new(&data), None, None).unwrap().to_string();

        assert_eq!(verify(&data, &digest(&data), &signature, &public_key), Ok(()));
        assert_eq!(verify(&data, &digest(&data).to_uppercase(), &signature, &public_key), Ok(()));

        let tampered = b"plume-lite 0.5.1!".to_vec();
        assert!(verify(&tampered, &digest(&data), &signature, &public_key).is_err(), "digest mismatch");
        let err = verify(&tampered, &digest(&tampered), &signature, &public_key).unwrap_err();
        assert!(err.contains("pas signée"), "a matching digest is not enough: {err}");

        let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let forged = minisign::sign(Some(&other.pk), &other.sk, Cursor::new(&data), None, None).unwrap().to_string();
        assert!(verify(&data, &digest(&data), &forged, &public_key).is_err(), "another key");
        assert!(verify(&data, &digest(&data), "garbage", &public_key).is_err());
        assert!(PublicKey::decode(PUBLIC_KEY).is_ok(), "the built-in key is valid");
    }

    #[test]
    fn saved_builds_carry_their_version() {
        assert_eq!(versioned_name("/download/plume-lite.exe", "0.5.1"), "plume-lite-0.5.1.exe");
        assert_eq!(
            versioned_name("/download/plume-lite-linux-x64.tar.gz", "0.5.1"),
            "plume-lite-linux-x64-0.5.1.tar.gz"
        );
    }
}
