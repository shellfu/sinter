package com.google.gson;

import static com.google.common.truth.Truth.assertThat;
import static org.junit.Assert.assertThrows;

import java.util.Arrays;
import org.junit.Test;

public final class AgentEvalJsonObjectGetOrDefaultTest {
  @Test
  public void testGetOrDefault() {
    JsonObject o = new JsonObject();
    o.addProperty("a", 1);
    o.add("n", JsonNull.INSTANCE);
    JsonPrimitive d = new JsonPrimitive("d");
    assertThat(o.getOrDefault("a", d).getAsInt()).isEqualTo(1);
    assertThat(o.getOrDefault("n", d)).isEqualTo(JsonNull.INSTANCE);
    assertThat(o.getOrDefault("missing", d)).isSameInstanceAs(d);
  }
}
