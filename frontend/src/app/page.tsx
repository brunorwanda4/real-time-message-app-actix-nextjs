import Link from 'next/link';

export default function HomePage() {
  return (
    <div className="min-h-screen bg-base-200 flex items-center justify-center p-6">
      <div className="card bg-base-100 shadow-2xl w-full max-w-md">
        <div className="card-body text-center">
          <h1 className="text-3xl font-bold mb-4">💬 Messenger Demo</h1>
          <p className="text-base-content/70 mb-8">Choose your real-time messaging technology</p>

          <div className="space-y-4">
            <Link href="/websocket" className="btn btn-primary btn-lg w-full">
              🔌 WebSocket Messenger
              <div className="badge badge-secondary ml-2">Bidirectional</div>
            </Link>

            <Link href="/sse" className="btn btn-accent btn-lg w-full">
              📡 SSE Messenger
              <div className="badge badge-info ml-2">Server Push</div>
            </Link>
          </div>

          <div className="mt-8 text-sm text-base-content/50">
            <p>Both implementations feature:</p>
            <ul className="mt-2 space-y-1">
              <li>✅ Real-time messaging</li>
              <li>✅ Message editing</li>
              <li>✅ Facebook-like UI</li>
              <li>✅ Connection status</li>
            </ul>
          </div>
        </div>
      </div>
    </div>
  );
}
