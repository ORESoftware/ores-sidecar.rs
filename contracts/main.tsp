import "@typespec/json-schema";
using TypeSpec.JsonSchema;

namespace OresSidecar;

@id("RuntimeUpdateProvider")
enum RuntimeUpdateProvider {
  oresRedisLruCache: "ores-redis-lru-cache",
}

@id("RuntimeUpdateRole")
enum RuntimeUpdateRole {
  server: "server",
}

@id("RuntimeUpdates")
model RuntimeUpdates {
  provider: RuntimeUpdateProvider;
  lruConfigPath: string;
  role: RuntimeUpdateRole;
  cache: string;
}

@id("Sidecar")
model Sidecar {
  name: string;
  enabled: boolean;
  bindIp: string;
  bindPort: uint16;
  loopbackOnly: boolean;
  runtimeNamespace: string;
  runtimeKeys: string[];
}

@id("SidecarConfig")
model SidecarConfig {
  protocol: "ores.sidecar-config.v1";
  runtimeUpdates: RuntimeUpdates;
  sidecars: Sidecar[];
}

@id("RuntimeValue")
model RuntimeValue {
  key: string;
  value: string;
}

@id("RuntimeSnapshotUpdate")
model RuntimeSnapshotUpdate {
  protocol: "ores.sidecar-runtime.v1";
  sidecar: string;
  revision: string;
  values: RuntimeValue[];
}
