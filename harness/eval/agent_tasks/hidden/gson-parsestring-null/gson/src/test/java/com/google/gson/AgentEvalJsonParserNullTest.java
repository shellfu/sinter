package com.google.gson;

import static com.google.common.truth.Truth.assertThat;
import static org.junit.Assert.assertThrows;

import java.util.Arrays;
import org.junit.Test;

public final class AgentEvalJsonParserNullTest {
  @Test
  public void testParseStringNull() {
    NullPointerException e = assertThrows(NullPointerException.class, () -> JsonParser.parseString(null));
    assertThat(e).hasMessageThat().isEqualTo("json == null");
  }
}
