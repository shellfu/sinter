package com.google.gson;

import static com.google.common.truth.Truth.assertThat;
import static org.junit.Assert.assertThrows;

import java.util.Arrays;
import org.junit.Test;

public final class AgentEvalJsonArrayNullTest {
  @Test
  public void testContainsAndRemoveNull() {
    JsonArray a = new JsonArray();
    a.add((JsonElement) null);
    a.add(1);
    assertThat(a.contains(null)).isTrue();
    assertThat(a.remove((JsonElement) null)).isTrue();
    assertThat(a.size()).isEqualTo(1);
    assertThat(a.contains(null)).isFalse();
    assertThat(a.remove((JsonElement) null)).isFalse();
  }
}
