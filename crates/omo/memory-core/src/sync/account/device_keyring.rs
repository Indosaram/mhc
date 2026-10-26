use crate::sync::{
    crypto::signatures::device_id_of,
    keyring_port::{
        DeviceKeyMaterial, DeviceKeyring, DeviceKeyringError, DeviceKeyringImpl,
        create_device_keyring,
    },
};
pub fn device_keyring_from_material(
    material: &DeviceKeyMaterial,
) -> Result<DeviceKeyringImpl, DeviceKeyringError> {
    let keyring = create_device_keyring(String::new(), material)?;
    let id = device_id_of(&keyring.public_keys().signing)
        .ok_or(DeviceKeyringError::InvalidKeyMaterial)?;
    create_device_keyring(id, material)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::keyring_port::generate_device_key_material;
    #[test]
    fn service_material_derives_principal_id() {
        let generated = generate_device_key_material().expect("material");
        let keyring = device_keyring_from_material(&generated.material).expect("keyring");
        assert_eq!(keyring.public_keys(), generated.public_keys);
        assert_eq!(
            Some(keyring.principal().id),
            device_id_of(&generated.public_keys.signing)
        );
    }
}
