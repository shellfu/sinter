package com.google.gson;

import static com.google.common.truth.Truth.assertThat;
import static org.junit.Assert.assertThrows;

import java.util.Arrays;
import org.junit.Test;

public final class AgentEvalNamingPolicyTest {
  @SuppressWarnings("unused")
  private static class Dummy {
    int someFieldName;
    int _someFieldName;
    int aURL;
  }

  @Test
  public void testUpperCaseWithDashes() throws Exception {
    FieldNamingPolicy p = FieldNamingPolicy.UPPER_CASE_WITH_DASHES;
    assertThat(p.translateName(Dummy.class.getDeclaredField("someFieldName"))).isEqualTo("SOME-FIELD-NAME");
    assertThat(p.translateName(Dummy.class.getDeclaredField("_someFieldName"))).isEqualTo("_SOME-FIELD-NAME");
    assertThat(p.translateName(Dummy.class.getDeclaredField("aURL"))).isEqualTo("A-U-R-L");
  }
}
