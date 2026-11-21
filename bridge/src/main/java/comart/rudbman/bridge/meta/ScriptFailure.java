package comart.rudbman.bridge.meta;

import comart.rudbman.bridge.BridgeException;

/**
 * A dialect profile's Groovy script that did not compile, or a hook of it that
 * threw ({@code docs/dialect-profiles.md}, §5.3).
 *
 * <p>Reported under the envelope kind {@code script}, with the script's file
 * and line in the message. Nothing in the bridge falls back to the base's
 * answer on one of these: {@link Attempt} and the backup's per-object catch
 * pass it on, so that a broken script fails the request it was serving rather
 * than looking like a working one.
 */
public final class ScriptFailure extends BridgeException {

    /** Serialized form version; the bridge never serializes these, but the compiler asks. */
    private static final long serialVersionUID = 1L;

    /**
     * @param message the profile, the file, the line and what went wrong
     * @param cause   what the script or Groovy threw, or {@code null}
     */
    ScriptFailure(String message, Throwable cause) {
        super("script", message, cause);
    }
}
