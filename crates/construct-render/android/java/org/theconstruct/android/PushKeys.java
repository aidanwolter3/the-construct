package org.theconstruct.android;

import android.content.Context;
import org.unifiedpush.android.connector.data.PublicKeySet;
import org.unifiedpush.android.connector.keys.KeyManager;

/**
 * The connector's key store, kept in Rust (`src/push.rs`): each instance's Web Push key pair and auth secret, and
 * decryption (RFC 8291). Only forwards, so the connector's own Tink-based keys are never used.
 */
final class PushKeys implements KeyManager {
    private final Context context;

    PushKeys(Context context) {
        this.context = context;
    }

    @Override
    public byte[] decrypt(String instance, byte[] sealed) {
        return NativePush.decrypt(context, instance, sealed);
    }

    @Override
    public void generate(String instance) {
        NativePush.generate(context, instance);
    }

    @Override
    public PublicKeySet getPublicKeySet(String instance) {
        String[] keys = NativePush.publicKeys(context, instance);
        return keys == null ? null : new PublicKeySet(keys[0], keys[1]);
    }

    @Override
    public boolean exists(String instance) {
        return NativePush.exists(context, instance);
    }

    @Override
    public void delete(String instance) {
        NativePush.delete(context, instance);
    }
}
