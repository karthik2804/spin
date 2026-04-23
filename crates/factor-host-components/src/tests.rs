#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::service::{
        create_host_engine, instantiate_host_component, load_host_component_from_bytes,
    };
    use wasmtime::component::Val;

    fn test_data_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/host-components-test")
    }

    fn cache_provider_wasm() -> Vec<u8> {
        let path = test_data_dir()
            .join("cache-provider/target/wasm32-wasip2/release/cache_provider.wasm");
        std::fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "Failed to read cache provider wasm at {}: {}. Build it first with: \
                 cd tests/host-components-test/cache-provider && cargo build --target wasm32-wasip2 --release",
                path.display(),
                e
            )
        })
    }

    #[test]
    fn test_load_cache_provider() {
        let engine = create_host_engine().unwrap();
        let bytes = cache_provider_wasm();
        let loaded = load_host_component_from_bytes(&engine, "cache", &bytes).unwrap();

        assert_eq!(loaded.name, "cache");
        assert!(
            !loaded.exported_interfaces.is_empty(),
            "cache provider should export at least one interface"
        );

        let store_iface = loaded
            .exported_interfaces
            .iter()
            .find(|i| i.name.contains("store"))
            .expect("cache provider should export a 'store' interface");

        assert!(store_iface.functions.contains(&"get".to_string()));
        assert!(store_iface.functions.contains(&"set".to_string()));
        assert!(store_iface.functions.contains(&"delete".to_string()));
        assert!(store_iface.functions.contains(&"exists".to_string()));
    }

    #[tokio::test]
    async fn test_cache_set_get() {
        let engine = create_host_engine().unwrap();
        let bytes = cache_provider_wasm();
        let loaded = load_host_component_from_bytes(&engine, "cache", &bytes).unwrap();
        let service = instantiate_host_component(&engine, &loaded, None).await.unwrap();

        let iface_name = loaded
            .exported_interfaces
            .iter()
            .find(|i| i.name.contains("store"))
            .unwrap()
            .name
            .clone();

        let mut svc = service.lock().await;

        // Set a value
        let mut results = [Val::Result(Ok(None))];
        svc.call_func(
            &iface_name,
            "set",
            &[
                Val::String("mystore".into()),
                Val::String("key1".into()),
                Val::List(vec![Val::U8(72), Val::U8(101), Val::U8(108), Val::U8(108), Val::U8(111)]), // "Hello"
                Val::Option(None), // no TTL
            ],
            &mut results,
        )
        .await
        .unwrap();

        // Check the result is Ok
        match &results[0] {
            Val::Result(Ok(_)) => {}
            other => panic!("expected Ok result from set, got {:?}", other),
        }

        // Get the value back
        let mut results = [Val::Result(Ok(None))];
        svc.call_func(
            &iface_name,
            "get",
            &[
                Val::String("mystore".into()),
                Val::String("key1".into()),
            ],
            &mut results,
        )
        .await
        .unwrap();

        match &results[0] {
            Val::Result(Ok(Some(val))) => {
                match val.as_ref() {
                    Val::Option(Some(inner)) => {
                        match inner.as_ref() {
                            Val::List(bytes) => {
                                let data: Vec<u8> = bytes.iter().map(|v| match v {
                                    Val::U8(b) => *b,
                                    _ => panic!("expected u8 in list"),
                                }).collect();
                                assert_eq!(data, b"Hello");
                            }
                            _ => panic!("expected list inside option"),
                        }
                    }
                    _ => panic!("expected option, got {:?}", val),
                }
            }
            other => panic!("expected Ok(Some(...)) from get, got {:?}", other),
        }

        // Check exists
        let mut results = [Val::Result(Ok(None))];
        svc.call_func(
            &iface_name,
            "exists",
            &[
                Val::String("mystore".into()),
                Val::String("key1".into()),
            ],
            &mut results,
        )
        .await
        .unwrap();

        match &results[0] {
            Val::Result(Ok(Some(val))) => {
                match val.as_ref() {
                    Val::Bool(true) => {}
                    other => panic!("expected true, got {:?}", other),
                }
            }
            other => panic!("expected Ok(Some(true)) from exists, got {:?}", other),
        }

        // Delete the key
        let mut results = [Val::Result(Ok(None))];
        svc.call_func(
            &iface_name,
            "delete",
            &[
                Val::String("mystore".into()),
                Val::String("key1".into()),
            ],
            &mut results,
        )
        .await
        .unwrap();

        // Verify it's gone
        let mut results = [Val::Result(Ok(None))];
        svc.call_func(
            &iface_name,
            "exists",
            &[
                Val::String("mystore".into()),
                Val::String("key1".into()),
            ],
            &mut results,
        )
        .await
        .unwrap();

        match &results[0] {
            Val::Result(Ok(Some(val))) => {
                match val.as_ref() {
                    Val::Bool(false) => {}
                    other => panic!("expected false after delete, got {:?}", other),
                }
            }
            other => panic!("expected Ok(Some(false)) after delete, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_cache_state_persistence() {
        let engine = create_host_engine().unwrap();
        let bytes = cache_provider_wasm();
        let loaded = load_host_component_from_bytes(&engine, "cache", &bytes).unwrap();
        let service = instantiate_host_component(&engine, &loaded, None).await.unwrap();

        let iface_name = loaded
            .exported_interfaces
            .iter()
            .find(|i| i.name.contains("store"))
            .unwrap()
            .name
            .clone();

        let mut svc = service.lock().await;

        // Set multiple values
        for i in 0..5 {
            let mut results = [Val::Result(Ok(None))];
            svc.call_func(
                &iface_name,
                "set",
                &[
                    Val::String("persistent-store".into()),
                    Val::String(format!("key-{}", i)),
                    Val::List(vec![Val::U8(i as u8)]),
                    Val::Option(None),
                ],
                &mut results,
            )
            .await
            .unwrap();
        }

        // Verify all values exist
        for i in 0..5 {
            let mut results = [Val::Result(Ok(None))];
            svc.call_func(
                &iface_name,
                "exists",
                &[
                    Val::String("persistent-store".into()),
                    Val::String(format!("key-{}", i)),
                ],
                &mut results,
            )
            .await
            .unwrap();

            match &results[0] {
                Val::Result(Ok(Some(val))) => match val.as_ref() {
                    Val::Bool(true) => {}
                    other => panic!("key-{} should exist, got {:?}", i, other),
                },
                other => panic!("unexpected result for key-{}: {:?}", i, other),
            }
        }
    }
}
