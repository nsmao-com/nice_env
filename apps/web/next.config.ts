import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // 静态导出：桌面窗口由 Tauri 资产协议加载，不依赖在线 Node 服务
  output: "export",
  images: { unoptimized: true },
  transpilePackages: ["@nsb/schema"],
  devIndicators: false,
};

export default nextConfig;
