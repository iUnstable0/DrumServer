import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.net.http.WebSocket;
import java.time.Duration;
import java.util.ArrayDeque;
import java.util.Deque;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.TimeUnit;

/**
 * Optional Java diagnostic probe for the drum-pi server.
 *
 * <p>This tool is intentionally kept out of the Rust server runtime. It uses
 * only the Java standard library to verify that a running drum-pi process
 * answers /health and accepts the harmless WebSocket protocol messages used
 * below.
 */
public final class DrumPiProbe {
    private static final String CLIENT_VERSION = "drum-java-probe/0.1.0";
    private static final String HELLO_JSON =
            "{\"type\":\"hello\",\"client_version\":\"" + CLIENT_VERSION + "\"}";
    private static final String DIAGNOSTICS_JSON = "{\"type\":\"diagnostics\"}";

    private DrumPiProbe() {
    }

    public static void main(String[] args) throws Exception {
        Config config = Config.parse(args);
        if (config.help()) {
            printUsage();
            return;
        }

        HttpClient client = HttpClient.newBuilder()
                .connectTimeout(Duration.ofMillis(config.timeoutMs()))
                .build();

        URI healthUri = URI.create("http://" + config.host() + ":" + config.port() + "/health");
        URI wsUri = URI.create("ws://" + config.host() + ":" + config.port() + "/ws");

        checkHealth(client, healthUri, config.timeoutMs());
        checkWebSocket(client, wsUri, config.timeoutMs());

        System.out.println("drum-pi probe succeeded");
    }

    private static void checkHealth(HttpClient client, URI uri, int timeoutMs) throws Exception {
        System.out.println("GET " + uri);
        HttpRequest request = HttpRequest.newBuilder(uri)
                .timeout(Duration.ofMillis(timeoutMs))
                .GET()
                .build();

        HttpResponse<String> response = client.send(request, HttpResponse.BodyHandlers.ofString());
        String body = response.body() == null ? "" : response.body().trim();
        if (response.statusCode() != 200 || !"ok".equals(body)) {
            throw new IllegalStateException(
                    "health check failed: HTTP " + response.statusCode() + " body=\"" + body + "\"");
        }
        System.out.println("health: ok");
    }

    private static void checkWebSocket(HttpClient client, URI uri, int timeoutMs) throws Exception {
        System.out.println("WS  " + uri);
        ProbeListener listener = new ProbeListener();
        WebSocket ws = client.newWebSocketBuilder()
                .connectTimeout(Duration.ofMillis(timeoutMs))
                .buildAsync(uri, listener)
                .get(timeoutMs, TimeUnit.MILLISECONDS);

        String ready = listener.nextMessage(timeoutMs);
        requireContains(ready, "\"type\":\"ready\"", "initial ready message");
        System.out.println("ready: " + ready);

        ws.sendText(HELLO_JSON, true).get(timeoutMs, TimeUnit.MILLISECONDS);
        String helloReply = listener.nextMessage(timeoutMs);
        requireContains(helloReply, "\"type\":\"ready\"", "hello response");
        System.out.println("hello: " + helloReply);

        ws.sendText(DIAGNOSTICS_JSON, true).get(timeoutMs, TimeUnit.MILLISECONDS);
        String diagnostics = listener.nextMessage(timeoutMs);
        requireContains(diagnostics, "\"type\":\"diagnostics\"", "diagnostics response");
        System.out.println("diagnostics: " + diagnostics);

        ws.sendClose(WebSocket.NORMAL_CLOSURE, "probe complete")
                .get(timeoutMs, TimeUnit.MILLISECONDS);
    }

    private static void requireContains(String message, String expected, String label) {
        if (!message.contains(expected)) {
            throw new IllegalStateException(label + " did not contain " + expected + ": " + message);
        }
    }

    private static void printUsage() {
        System.out.println("""
                Usage: java tools/java/DrumPiProbe.java [options]

                Options:
                  --host <host>          Server host or IP address. Default: 127.0.0.1
                  --port <port>          Server port. Default: 9090
                  --timeout-ms <millis>  Per-operation timeout. Default: 2000
                  --help                 Show this help text.

                Example:
                  java tools/java/DrumPiProbe.java --host 127.0.0.1 --port 9090
                """);
    }

    private record Config(String host, int port, int timeoutMs, boolean help) {
        static Config parse(String[] args) {
            String host = "127.0.0.1";
            int port = 9090;
            int timeoutMs = 2000;
            boolean help = false;

            for (int i = 0; i < args.length; i++) {
                String arg = args[i];
                switch (arg) {
                    case "--host" -> host = requireValue(args, ++i, arg);
                    case "--port" -> port = parseInt(requireValue(args, ++i, arg), arg, 1, 65535);
                    case "--timeout-ms" -> timeoutMs = parseInt(requireValue(args, ++i, arg), arg, 1, 120_000);
                    case "--help", "-h" -> help = true;
                    default -> throw new IllegalArgumentException("unknown option: " + arg);
                }
            }

            return new Config(host, port, timeoutMs, help);
        }

        private static String requireValue(String[] args, int index, String option) {
            if (index >= args.length || args[index].startsWith("--")) {
                throw new IllegalArgumentException(option + " requires a value");
            }
            return args[index];
        }

        private static int parseInt(String raw, String option, int min, int max) {
            try {
                int value = Integer.parseInt(raw);
                if (value < min || value > max) {
                    throw new IllegalArgumentException(
                            option + " must be between " + min + " and " + max + ": " + raw);
                }
                return value;
            } catch (NumberFormatException e) {
                throw new IllegalArgumentException(option + " must be an integer: " + raw, e);
            }
        }
    }

    private static final class ProbeListener implements WebSocket.Listener {
        private final Deque<String> queue = new ArrayDeque<>();
        private final StringBuilder current = new StringBuilder();
        private Throwable failure;

        synchronized String nextMessage(int timeoutMs) throws InterruptedException {
            long deadline = System.nanoTime() + TimeUnit.MILLISECONDS.toNanos(timeoutMs);
            while (queue.isEmpty()) {
                if (failure != null) {
                    throw new IllegalStateException("WebSocket failed", failure);
                }
                long remainingNs = deadline - System.nanoTime();
                if (remainingNs <= 0) {
                    throw new IllegalStateException("timed out waiting for WebSocket message");
                }
                TimeUnit.NANOSECONDS.timedWait(this, remainingNs);
            }
            return queue.remove();
        }

        @Override
        public CompletionStage<?> onText(WebSocket webSocket, CharSequence data, boolean last) {
            synchronized (this) {
                current.append(data);
                if (last) {
                    queue.add(current.toString());
                    current.setLength(0);
                    notifyAll();
                }
            }
            webSocket.request(1);
            return CompletableFuture.completedFuture(null);
        }

        @Override
        public void onOpen(WebSocket webSocket) {
            webSocket.request(1);
        }

        @Override
        public void onError(WebSocket webSocket, Throwable error) {
            synchronized (this) {
                failure = error;
                notifyAll();
            }
        }
    }
}
