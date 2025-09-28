import Navigation from '@/components/Navigation';
import type { Metadata } from 'next';
import { Inter } from 'next/font/google';
import './globals.css';

const inter = Inter({ subsets: ['latin'] });

export const metadata: Metadata = {
  title: 'Messenger Demo - WebSocket vs SSE',
  description: 'Real-time messaging comparison between WebSocket and Server-Sent Events',
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" data-theme="forest">
      <body className={inter.className}>
        <Navigation />
        {children}
      </body>
    </html>
  );
}
