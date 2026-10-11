package comart.rudbman.bridge;

import com.google.gson.JsonObject;
import comart.rudbman.bridge.codec.LobSink;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.Reader;
import java.io.Writer;
import java.nio.ByteBuffer;
import java.nio.channels.SeekableByteChannel;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Blob;
import java.sql.Clob;
import java.sql.SQLException;
import java.util.HashMap;
import java.util.Map;

/** Cursor-owned snapshots: a driver's LOB locator may expire at the next row. */
final class LobStore implements LobSink {
    static final int MAX_CHUNK = 1024 * 1024;
    private final Map<Long, Path> files = new HashMap<>();
    private long nextId = 1;

    @Override
    public long registerLob(long row, int column, long size, boolean binary, Object value)
            throws SQLException {
        Path path = null;
        try {
            // createTempFile creates an owner-only file on POSIX systems.
            path = Files.createTempFile("rudbman-lob-", ".tmp");
            if (binary) {
                try (InputStream in = value instanceof InputStream
                        ? (InputStream) value : ((Blob) value).getBinaryStream();
                     OutputStream out = Files.newOutputStream(path)) {
                    in.transferTo(out);
                }
            } else {
                try (Reader in = value instanceof Reader
                        ? (Reader) value : ((Clob) value).getCharacterStream();
                     Writer out = Files.newBufferedWriter(path, StandardCharsets.UTF_8)) {
                    in.transferTo(out);
                }
            }
            long id = nextId++;
            files.put(id, path);
            return id;
        } catch (IOException | SQLException | RuntimeException e) {
            if (path != null) delete(path);
            throw new SQLException("cannot snapshot LOB at row " + row + ", column " + column, e);
        }
    }

    /** Offsets and lengths are bytes; character LOBs are transported as UTF-8. */
    byte[] read(JsonObject request) throws IOException {
        long id = request.has("id") ? request.get("id").getAsLong() : 0;
        long offset = request.has("offset") ? request.get("offset").getAsLong() : -1;
        int length = Json.i32(request, "length", 0);
        if (offset < 0 || length < 1 || length > MAX_CHUNK) {
            throw new BridgeException("protocol", "LOB_READ requires a nonnegative byte offset and length 1..1048576");
        }
        Path path = files.get(id);
        if (path == null) {
            throw new BridgeException("protocol", "unknown LOB " + id);
        }
        try (SeekableByteChannel channel = Files.newByteChannel(path)) {
            if (offset > channel.size()) {
                throw new BridgeException("protocol", "LOB_READ offset exceeds the value length");
            }
            channel.position(offset);
            ByteBuffer bytes = ByteBuffer.allocate((int) Math.min(length, channel.size() - offset));
            while (bytes.hasRemaining() && channel.read(bytes) >= 0) { /* fill the chunk */ }
            return bytes.array();
        }
    }

    void clear() {
        files.values().forEach(LobStore::delete);
        files.clear();
    }

    private static void delete(Path path) {
        try {
            Files.deleteIfExists(path);
        } catch (IOException e) {
            // A transient filesystem refusal must not stop the cursor closing.
            path.toFile().deleteOnExit();
        }
    }
}
