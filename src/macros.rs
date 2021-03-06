#[macro_export]
macro_rules! redis_command {
    ($ctx:expr,
     $command_name:expr,
     $command_handler:expr,
     $command_flags:expr,
     $firstkey:expr,
     $lastkey:expr,
     $keystep:expr) => {{
        let name = CString::new($command_name).unwrap();
        let flags = CString::new($command_flags).unwrap();

        /////////////////////
        extern "C" fn do_command(
            ctx: *mut $crate::raw::RedisModuleCtx,
            argv: *mut *mut $crate::raw::RedisModuleString,
            argc: c_int,
        ) -> c_int {
            let context = $crate::Context::new(ctx);

            // catch any panics to avoid crashing redis
            let response = std::panic::catch_unwind(|| {
                let args_decoded: Result<Vec<_>, $crate::RedisError> =
                    unsafe { slice::from_raw_parts(argv, argc as usize) }
                        .into_iter()
                        .map(|&arg| {
                            $crate::RedisString::from_ptr(arg)
                                .map(|v| v.to_owned())
                                .map_err(|_| {
                                    $crate::RedisError::Str("UTF8 encoding error in handler args")
                                })
                        })
                        .collect();

                args_decoded
                    .map(|args| $command_handler(&context, args))
                    .unwrap_or_else(|e| Err(e))
            });

            let response = match response {
                Ok(response) => response,
                Err(_) => Err($crate::RedisError::String(format!(
                    "caught panic in redis command handler for {}",
                    $command_name,
                ))),
            };

            context.reply(response) as c_int
        }
        /////////////////////

        if unsafe {
            $crate::raw::RedisModule_CreateCommand.unwrap()(
                $ctx,
                name.as_ptr(),
                Some(do_command),
                flags.as_ptr(),
                $firstkey,
                $lastkey,
                $keystep,
            )
        } == $crate::raw::Status::Err as c_int
        {
            return $crate::raw::Status::Err as c_int;
        }
    }};
}

#[cfg(feature = "experimental-api")]
#[macro_export]
macro_rules! redis_event_handler {
    (
        $ctx: expr,
        $event_type: expr,
        $event_handler: expr
    ) => {{
        extern "C" fn handle_event(
            ctx: *mut $crate::raw::RedisModuleCtx,
            event_type: c_int,
            event: *const c_char,
            key: *mut $crate::raw::RedisModuleString,
        ) -> c_int {
            let context = $crate::Context::new(ctx);

            let redis_key = $crate::RedisString::from_ptr(key).unwrap();
            let event_str = unsafe { CStr::from_ptr(event) };
            $event_handler(
                &context,
                $crate::NotifyEvent::from_bits_truncate(event_type),
                event_str.to_str().unwrap(),
                redis_key,
            );

            $crate::raw::Status::Ok as c_int
        }

        if unsafe {
            $crate::raw::RedisModule_SubscribeToKeyspaceEvents.unwrap()(
                $ctx,
                $event_type.bits(),
                Some(handle_event),
            )
        } == $crate::raw::Status::Err as c_int
        {
            return $crate::raw::Status::Err as c_int;
        }
    }};
}

#[macro_export]
macro_rules! redis_module {
    (
        name: $module_name:expr,
        version: $module_version:expr,
        data_types: [
            $($data_type:ident),* $(,)*
        ],
        $(init: $init_func:ident,)* $(,)*
        $(deinit: $deinit_func:ident,)* $(,)*
        commands: [
            $([
                $name:expr,
                $command:expr,
                $flags:expr,
                $firstkey:expr,
                $lastkey:expr,
                $keystep:expr
              ]),* $(,)*
        ] $(,)*
        $(event_handlers: [
            $([
                $(@$event_type:ident) +:
                $event_handler:expr
            ]),* $(,)*
        ])?
    ) => {
        #[no_mangle]
        #[allow(non_snake_case)]
        pub extern "C" fn RedisModule_OnLoad(
            ctx: *mut $crate::raw::RedisModuleCtx,
            _argv: *mut *mut $crate::raw::RedisModuleString,
            _argc: std::os::raw::c_int,
        ) -> std::os::raw::c_int {
            use std::os::raw::{c_int, c_char};
            use std::ffi::{CString, CStr};
            use std::slice;

            use $crate::raw;
            use $crate::RedisString;

            // We use a statically sized buffer to avoid allocating.
            // This is needed since we use a custom allocator that relies on the Redis allocator,
            // which isn't yet ready at this point.
            let mut name_buffer = [0; 64];
            unsafe {
                std::ptr::copy(
                    $module_name.as_ptr(),
                    name_buffer.as_mut_ptr(),
                    $module_name.len(),
                );
            }

            let module_version = $module_version as c_int;

            if unsafe { raw::Export_RedisModule_Init(
                ctx,
                name_buffer.as_ptr() as *const c_char,
                module_version,
                raw::REDISMODULE_APIVER_1 as c_int,
            ) } == raw::Status::Err as c_int { return raw::Status::Err as c_int; }

            // catch any panics to avoid crashing redis
            let res = std::panic::catch_unwind(|| {
              $(
                if $init_func(ctx) == raw::Status::Err as c_int {
                  return raw::Status::Err as c_int;
                }
              )*

              $(
                 if (&$data_type).create_data_type(ctx).is_err() {
                   return raw::Status::Err as c_int;
                 }
               )*

               $(
                   redis_command!(ctx, $name, $command, $flags, $firstkey, $lastkey, $keystep);
               )*

               raw::Status::Ok as c_int
            });

            $(
                $(
                    redis_event_handler!(ctx, $(raw::NotifyEvent::$event_type |)+ raw::NotifyEvent::empty(), $event_handler);
                )*
            )?

            match res {
                Ok(res) => res,
                Err(_) => raw::Status::Err as c_int,
            }
        }

        #[no_mangle]
        #[allow(non_snake_case)]
        pub extern "C" fn RedisModule_OnUnload(
            ctx: *mut $crate::raw::RedisModuleCtx
        ) -> std::os::raw::c_int {
            let res = std::panic::catch_unwind(|| {
              $(
                  if $deinit_func(ctx) == $crate::raw::Status::Err as c_int {
                      return $crate::raw::Status::Err as c_int;
                  }
              )*
            });

            match res {
                Ok(_) => $crate::raw::Status::Ok as c_int,
                Err(_) => $crate::raw::Status::Err as c_int,
            }
        }
    }
}
