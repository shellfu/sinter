package com.google.gson;

import static com.google.common.truth.Truth.assertThat;
import static org.junit.Assert.assertThrows;

import java.util.Arrays;
import org.junit.Test;

public final class AgentEvalJsonArrayAddAllTest {
  @Test
  public void testAddAllCollection() {
    JsonArray a = new JsonArray();
    a.addAll(Arrays.asList(new JsonPrimitive(1), null, new JsonPrimitive("x")));
    assertThat(a.size()).isEqualTo(3);
    assertThat(a.get(1)).isEqualTo(JsonNull.INSTANCE);
    assertThat(a.get(2).getAsString()).isEqualTo("x");
  }
}
